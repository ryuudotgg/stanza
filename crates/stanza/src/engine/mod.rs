pub mod attach;
pub mod directives;
pub mod doc;
pub mod edits;
pub mod gaps;
pub mod model;
pub mod rules;
pub mod types;
pub mod widths;

use oxc_allocator::Allocator;
use oxc_parser::ParserReturn;

use crate::languages::FALLBACK_LAYOUT;
use crate::languages::javascript::parse::{parse, rejection};
use crate::languages::javascript::scan::{comments, scan};

use doc::{document, parse_finding};
use edits::apply_lines;
use gaps::{Spacing, spacing};
use types::{FileResult, Finding, Fixed, Mode, compare_findings};

fn spaced(
    path: &str,
    text: &str,
    parsed: &ParserReturn,
    mode: Mode,
) -> (Vec<Finding>, Option<String>) {
    let doc = document(text, comments(text, &parsed.program));
    let scanned = scan(&doc, &parsed.program, FALLBACK_LAYOUT);
    let Spacing { edits, findings } =
        spacing(path, &doc, &scanned.lists, &scanned.frozen, &|_, _| true);

    let edited = !edits.delete_lines.is_empty() || !edits.insert_after.is_empty();
    let fixed = (mode == Mode::Fix && edited).then(|| apply_lines(&doc, &edits));
    (findings, fixed)
}

fn remaining(findings: Vec<Finding>) -> Vec<Finding> {
    findings.into_iter().filter(|item| !item.fixable).collect()
}

fn reparsed(path: &str, text: String) -> FileResult {
    let allocator = Allocator::default();
    let parsed = parse(&allocator, path, &text);
    if let Some(error) = parsed.diagnostics.first() {
        let error = Some(error.message.to_string());
        return FileResult::fixed(Vec::new(), Fixed { text, error });
    }

    let findings = remaining(spaced(path, &text, &parsed, Mode::Check).0);
    FileResult::fixed(findings, Fixed { text, error: None })
}

pub fn process_file(path: &str, text: &str, mode: Mode) -> FileResult {
    let allocator = Allocator::default();
    let parsed = parse(&allocator, path, text);
    if let Some(rejected) = rejection(text, &parsed) {
        return FileResult {
            findings: vec![parse_finding(path, text, rejected.start, rejected.message)],
            parse_error: true,
            fixed: None,
        };
    }

    let (findings, fixed) = spaced(path, text, &parsed, mode);

    let mut result = match (mode, fixed) {
        (Mode::Fix, Some(fixed)) if fixed != text => reparsed(path, fixed),
        (Mode::Fix, _) => FileResult::unchanged(remaining(findings)),
        (Mode::Check, _) => FileResult::unchanged(findings),
    };

    result.findings.sort_by(compare_findings);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step::format_text;

    const SPREAD: &str = "function f() {\n  const x = {\n    a,\n  };\n  log(x);\n\n\n  b();\n}\n";

    const SPACED: &str =
        "function f() {\n  const x = {\n    a,\n  };\n\n  log(x);\n\n\n  b();\n}\n";

    fn fixed(text: &str) -> Option<String> {
        format_text("a.ts", Ok(text.to_owned()), Mode::Fix).fixed
    }

    #[test]
    fn fix_writes_the_spacing_edits() {
        assert_eq!(fixed(SPREAD).as_deref(), Some(SPACED));
    }

    #[test]
    fn fix_leaves_clean_text_unchanged() {
        assert_eq!(fixed("function f() {\n  a();\n  b();\n}\n"), None);
    }

    #[test]
    fn fix_keeps_the_byte_order_mark_and_carriage_returns() {
        let source = format!("\u{feff}{}", SPREAD.replace('\n', "\r\n"));
        let expected = format!("\u{feff}{}", SPACED.replace('\n', "\r\n"));
        assert_eq!(fixed(&source), Some(expected));
    }

    #[test]
    fn check_reports_what_fix_removes() {
        let checked = process_file("a.ts", SPREAD, Mode::Check);
        let fixed = process_file("a.ts", SPREAD, Mode::Fix);
        assert!(checked.findings.iter().any(|finding| finding.fixable));
        assert!(fixed.findings.iter().all(|finding| !finding.fixable));
        assert!(fixed.fixed.is_some_and(|fixed| fixed.error.is_none()));
    }
}
