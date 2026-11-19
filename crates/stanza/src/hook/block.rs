use std::fmt::Write;

use crate::format::Findings;
use crate::paths::shown;

const SHOWN: usize = 12;

pub fn block_reason(findings: &Findings, rewritten: &[String]) -> Option<String> {
    if findings.is_empty() {
        return None;
    }

    let mut text = String::from("These need a hand fix in the files you changed:\n");
    let mut left = SHOWN;
    let mut total = 0;
    for file in findings.files() {
        total += file.findings.len();
        if left == 0 {
            continue;
        }

        let _ = write!(text, "\n{}\n", shown(&file.path));
        for finding in file.findings.iter().take(left) {
            let _ = writeln!(
                text,
                "  line {}: {} ({})",
                finding.line,
                finding.message,
                finding.rule.id()
            );
        }

        left -= file.findings.len().min(left);
    }

    if total > SHOWN {
        let _ = writeln!(text, "  ... and {} more", total - SHOWN);
    }

    text.push('\n');
    if !rewritten.is_empty() {
        let mut paths: Vec<&str> = rewritten.iter().map(String::as_str).collect();
        paths.sort_unstable();
        let names: Vec<_> = paths.into_iter().map(shown).collect();
        text.push_str(&sentence("rewrote", &names));
        text.push('\n');
    }

    text.push_str(
        "Fix the findings above, then reply again. `stanza explain <file>:<line>` shows why a rule applies.",
    );

    Some(text)
}

pub fn reread_line(verb: &str, paths: &[String]) -> String {
    let mut paths = paths.to_vec();
    paths.sort();
    sentence(verb, &paths)
}

fn sentence(verb: &str, names: &[impl AsRef<str>]) -> String {
    let names: Vec<&str> = names.iter().map(AsRef::as_ref).collect();
    let (names, pronoun) = match names.as_slice() {
        [only] => ((*only).to_owned(), "it"),
        [head @ .., last] => (format!("{} and {last}", head.join(", ")), "them"),
        [] => (String::new(), "them"),
    };

    format!("stanza {verb} {names}, so read {pronoun} again before editing.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::rules::Rule;
    use crate::engine::types::{FileFindings, Finding};

    fn finding(line: usize, rule: Rule, message: &str) -> Finding {
        Finding {
            line,
            col: 3,
            rule,
            message: message.to_owned(),
            fixable: false,
        }
    }

    fn wall(line: usize, statements: usize) -> Finding {
        let message = format!("split these {statements} statements into steps with a blank line");
        finding(line, Rule::Wall, &message)
    }

    fn findings(files: &[(&str, Vec<Finding>)]) -> Findings {
        Findings::new(
            files
                .iter()
                .map(|(path, findings)| FileFindings {
                    path: (*path).to_owned(),
                    findings: findings.clone(),
                })
                .collect(),
        )
    }

    fn names(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn reread_line_names_files_in_byte_order() {
        let paths = names(&["b.ts", "/outside/z.ts", "B.ts"]);
        assert_eq!(
            reread_line("formatted", &paths),
            "stanza formatted /outside/z.ts, B.ts and b.ts, so read them again before editing."
        );
    }

    #[test]
    fn block_reason_is_absent_without_findings_even_when_files_were_rewritten() {
        assert_eq!(
            block_reason(&Findings::default(), &names(&["bodies.ts"])),
            None
        );
    }

    #[test]
    fn block_reason_groups_findings_by_file_and_names_every_rewritten_file() {
        let spacing = finding(
            10,
            Rule::BlockSpacing,
            "add a blank line above this block, or join it to the step above",
        );

        let found = findings(&[
            ("src/users.ts", vec![wall(4, 7)]),
            ("src/report.ts", vec![spacing, wall(2, 8)]),
        ]);

        assert_eq!(
            block_reason(&found, &names(&["src/orders.ts"])).unwrap(),
            "These need a hand fix in the files you changed:

src/report.ts
  line 2: split these 8 statements into steps with a blank line (wall)
  line 10: add a blank line above this block, or join it to the step above (block-spacing)

src/users.ts
  line 4: split these 7 statements into steps with a blank line (wall)

stanza rewrote src/orders.ts, so read it again before editing.
Fix the findings above, then reply again. `stanza explain <file>:<line>` shows why a rule applies."
        );
    }

    #[test]
    fn block_reason_reads_a_failure_like_any_finding() {
        let parse = finding(1, Rule::Parse, "Unexpected token");
        assert_eq!(
            block_reason(&findings(&[("broken.ts", vec![parse])]), &[]).unwrap(),
            "These need a hand fix in the files you changed:

broken.ts
  line 1: Unexpected token (parse)

Fix the findings above, then reply again. `stanza explain <file>:<line>` shows why a rule applies."
        );
    }

    #[test]
    fn block_reason_shows_twelve_findings_then_counts_the_rest() {
        let first: Vec<Finding> = (1..=10).map(|line| wall(line * 10, 6)).collect();
        let second: Vec<Finding> = (1..=4).map(|line| wall(line * 10, 6)).collect();
        let third = vec![wall(5, 6)];
        let found = findings(&[("a.ts", first), ("b.ts", second), ("c.ts", third)]);

        let reason = block_reason(&found, &names(&["c.ts"])).unwrap();
        assert_eq!(
            reason
                .lines()
                .filter(|line| line.starts_with("  line "))
                .count(),
            12
        );
        assert!(reason.contains(
            "\nb.ts\n  line 10: split these 6 statements into steps with a blank line (wall)\n  line 20: split these 6 statements into steps with a blank line (wall)\n  ... and 3 more\n\nstanza rewrote c.ts,"
        ));
        assert!(!reason.contains("\nc.ts\n"));
    }

    #[test]
    fn block_reason_prints_no_header_once_the_cap_ends_a_file() {
        let first: Vec<Finding> = (1..=12).map(|line| wall(line * 10, 6)).collect();
        let found = findings(&[("a.ts", first), ("b.ts", vec![wall(2, 8), wall(20, 6)])]);

        let reason = block_reason(&found, &[]).unwrap();
        assert!(reason.contains("  line 120: split these 6 statements into steps with a blank line (wall)\n  ... and 2 more\n\nFix the findings above"));
        assert!(!reason.contains("b.ts"));
    }

    #[test]
    fn block_reason_adds_no_count_at_exactly_twelve() {
        let twelve: Vec<Finding> = (1..=12).map(|line| wall(line * 10, 6)).collect();
        let reason = block_reason(&findings(&[("a.ts", twelve)]), &[]).unwrap();
        assert!(!reason.contains("... and"));
        assert!(reason.contains("(wall)\n\nFix the findings above"));
    }

    #[test]
    fn block_reason_quotes_paths_as_finding_rows_do() {
        let found = findings(&[("n\nl.ts", vec![wall(2, 8)])]);
        let reason = block_reason(&found, &names(&["z\"q.ts", "a.ts"])).unwrap();

        assert!(reason.contains("\n\"n\\nl.ts\"\n  line 2: "));
        assert!(reason.contains(
            "\nstanza rewrote a.ts and \"z\\\"q.ts\", so read them again before editing.\n"
        ));
    }

    #[test]
    fn block_reason_names_rewritten_files_in_byte_order() {
        let found = findings(&[("wall.ts", vec![wall(2, 8)])]);
        let reason = block_reason(&found, &names(&["b.ts", "B.ts", "a.ts"])).unwrap();
        assert!(reason.contains("\nstanza rewrote B.ts, a.ts and b.ts, so read them again"));
    }
}
