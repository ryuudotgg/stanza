pub mod attach;
pub mod directives;
pub mod doc;
pub mod gaps;
pub mod model;
pub mod types;

use oxc_allocator::Allocator;

use crate::languages::javascript::parse::{parse, rejection};
use crate::languages::javascript::scan::{comments, scan};

use doc::{document, parse_finding};
use gaps::spacing;
use types::{FileResult, Mode, compare_findings};

pub fn process_file(path: &str, text: &str, mode: Mode) -> FileResult {
    let allocator = Allocator::default();
    let parsed = parse(&allocator, path, text);
    if let Some(rejected) = rejection(text, &parsed) {
        return FileResult {
            findings: vec![parse_finding(path, text, rejected.start, rejected.message)],
            parse_error: true,
        };
    }

    let doc = document(text, comments(text, &parsed.program));
    let scanned = scan(&doc, &parsed.program);
    let mut findings = spacing(path, &doc, &scanned.lists, &scanned.frozen);
    if mode == Mode::Fix {
        findings.retain(|finding| !finding.fixable);
    }

    findings.sort_by(compare_findings);
    FileResult {
        findings,
        parse_error: false,
    }
}
