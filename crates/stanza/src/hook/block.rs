use std::collections::HashSet;

use crate::engine::rules::{RULES, Rule};
use crate::engine::types::Located;

fn failure(rule: Rule) -> Option<&'static str> {
    match rule {
        Rule::Parse => Some("could not read or parse this file"),
        Rule::Error => Some("stanza failed on this file"),
        Rule::Write => Some("could not write the fixes to this file"),
        _ => None,
    }
}

pub fn block_reason(findings: &[Located], rewritten: &[String]) -> Option<String> {
    if findings.is_empty() {
        return None;
    }

    let shown = &findings[..findings.len().min(12)];
    let mut lines = vec!["stanza could not fix these in the files you changed:".to_owned()];
    for &Located { path, finding } in shown {
        let message = match failure(finding.rule) {
            Some(failure) => format!("{failure}: {}", finding.message),
            None => format!("{} {}", finding.rule.id(), finding.message),
        };

        lines.push(format!(
            "  {path}:{}:{} {message}",
            finding.line, finding.col
        ));
    }

    if findings.len() > shown.len() {
        lines.push(format!("  ... and {} more", findings.len() - shown.len()));
    }

    let rules: Vec<String> = RULES
        .iter()
        .filter(|entry| {
            findings
                .iter()
                .any(|located| located.finding.rule == entry.rule)
        })
        .map(|entry| format!("{}: {}", entry.rule.id(), entry.summary))
        .collect();

    let mut sections = vec![lines.join("\n")];
    if !rules.is_empty() {
        sections.push(rules.join("\n"));
    }

    sections.push("Fix those findings, then reply again.".to_owned());

    let paths: HashSet<&str> = findings.iter().map(|located| located.path).collect();
    let reread: Vec<String> = rewritten
        .iter()
        .filter(|path| paths.contains(path.as_str()))
        .cloned()
        .collect();

    if !reread.is_empty() {
        sections.push(reread_line("rewrote", &reread));
    }

    Some(sections.join("\n\n"))
}

pub fn reread_line(verb: &str, paths: &[String]) -> String {
    let mut paths = paths.to_vec();
    paths.sort();

    let (names, pronoun) = match paths.as_slice() {
        [only] => (only.clone(), "it"),
        [head @ .., last] => (format!("{} and {last}", head.join(", ")), "them"),
        [] => (String::new(), "them"),
    };

    format!("stanza {verb} {names}, so read {pronoun} again before editing.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::types::Finding;

    fn finding(rule: Rule) -> Finding {
        Finding {
            line: 2,
            col: 3,
            rule,
            message: match rule {
                Rule::Parse => "Unexpected token",
                _ => "split these 8 statements into steps with a blank line",
            }
            .to_owned(),
            fixable: false,
        }
    }

    fn at<'a>(path: &'a str, finding: &'a Finding) -> Located<'a> {
        Located { path, finding }
    }

    fn summary(rule: Rule) -> &'static str {
        RULES
            .iter()
            .find(|entry| entry.rule == rule)
            .unwrap()
            .summary
    }

    #[test]
    fn reread_line_names_files_in_byte_order() {
        let paths = names(&["b.ts", "/outside/z.ts", "B.ts"]);
        assert_eq!(
            reread_line("formatted", &paths),
            "stanza formatted /outside/z.ts, B.ts and b.ts, so read them again before editing."
        );
    }

    fn names(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn block_reason_is_absent_without_findings_even_when_files_were_rewritten() {
        assert_eq!(block_reason(&[], &names(&["bodies.ts"])), None);
    }

    #[test]
    fn block_reason_prints_the_exact_finding_summary_and_instruction() {
        assert_eq!(
            block_reason(&[at("wall.ts", &finding(Rule::Wall))], &[]),
            Some(format!(
                "stanza could not fix these in the files you changed:\n  wall.ts:2:3 wall split these 8 statements into steps with a blank line\n\nwall: {}\n\nFix those findings, then reply again.",
                summary(Rule::Wall)
            ))
        );
    }

    #[test]
    fn block_reason_caps_finding_lines_at_twelve_and_reports_the_remaining_count() {
        let wall = finding(Rule::Wall);
        let paths: Vec<String> = (0..14).map(|index| format!("file{index}.ts")).collect();
        let findings: Vec<Located> = paths.iter().map(|path| at(path, &wall)).collect();

        let reason = block_reason(&findings, &names(&["file12.ts"])).unwrap();
        assert_eq!(
            reason
                .lines()
                .filter(|line| line.starts_with("  file"))
                .count(),
            12
        );
        assert!(reason.contains("  file11.ts:2:3 wall "));
        assert!(!reason.contains("  file12.ts"));
        assert!(reason.ends_with("stanza rewrote file12.ts, so read it again before editing."));
        assert!(reason.contains("\n  ... and 2 more\n\n"));
        assert!(
            !block_reason(&findings[..12], &[])
                .unwrap()
                .contains("... and")
        );
        assert!(
            !block_reason(&findings, &names(&["other.ts"]))
                .unwrap()
                .contains("stanza rewrote")
        );
    }

    #[test]
    fn block_reason_gives_parse_findings_their_own_message_without_a_rule_summary() {
        let parse = Finding {
            line: 1,
            col: 10,
            ..finding(Rule::Parse)
        };

        assert_eq!(
            block_reason(&[at("broken.ts", &parse)], &[]).unwrap(),
            "stanza could not fix these in the files you changed:\n  broken.ts:1:10 could not read or parse this file: Unexpected token\n\nFix those findings, then reply again."
        );
    }

    #[test]
    fn block_reason_tells_an_engine_failure_apart_from_a_parse_failure() {
        let failed = Finding {
            rule: Rule::Error,
            message: "RangeError".to_owned(),
            ..finding(Rule::Parse)
        };

        assert!(
            block_reason(&[at("deep.ts", &failed)], &[])
                .unwrap()
                .contains("deep.ts:2:3 stanza failed on this file: RangeError")
        );
    }

    #[test]
    fn block_reason_lists_each_rule_once_in_catalog_order() {
        let (wall, steps) = (finding(Rule::Wall), finding(Rule::BlockSpacing));
        let findings = [
            at("wall.ts", &wall),
            at("steps.ts", &steps),
            at("other.ts", &wall),
        ];

        let reason = block_reason(&findings, &[]).unwrap();
        assert_eq!(
            reason.split("\n\n").nth(1),
            Some(
                format!(
                    "block-spacing: {}\nwall: {}",
                    summary(Rule::BlockSpacing),
                    summary(Rule::Wall)
                )
                .as_str()
            )
        );
    }

    #[test]
    fn block_reason_names_only_rewritten_files_with_shown_findings() {
        let found = finding(Rule::Wall);
        let wall = [at("wall.ts", &found)];
        assert!(
            !block_reason(&wall, &names(&["bodies.ts"]))
                .unwrap()
                .contains("stanza rewrote")
        );
        assert!(
            block_reason(&wall, &names(&["bodies.ts", "wall.ts"]))
                .unwrap()
                .ends_with("\n\nstanza rewrote wall.ts, so read it again before editing.")
        );

        let findings = [at("a.ts", &found), at("b.ts", &found), at("c.ts", &found)];

        assert!(
            block_reason(&findings, &names(&["a.ts", "b.ts"]))
                .unwrap()
                .ends_with("\n\nstanza rewrote a.ts and b.ts, so read them again before editing.")
        );
        assert!(
            block_reason(&findings, &names(&["a.ts", "b.ts", "c.ts", "clean.ts"]))
                .unwrap()
                .ends_with(
                    "\n\nstanza rewrote a.ts, b.ts and c.ts, so read them again before editing."
                )
        );
    }
}
