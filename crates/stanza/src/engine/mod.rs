pub mod doc;
pub mod types;

use crate::languages::javascript::parse::rejection;

use doc::parse_finding;
use types::FileResult;

pub fn process_file(path: &str, text: &str) -> FileResult {
    match rejection(path, text) {
        Some(rejected) => FileResult {
            findings: vec![parse_finding(path, text, rejected.start, rejected.message)],
            parse_error: true,
        },
        None => FileResult {
            findings: Vec::new(),
            parse_error: false,
        },
    }
}
