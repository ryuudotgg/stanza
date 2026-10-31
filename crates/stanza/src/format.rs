use std::fs;

use crate::engine::types::{Finding, Mode, compare_findings};
use crate::files::with_code;
use crate::step::{Decoded, decode, format_text};

pub fn read_text(path: &str) -> Decoded {
    match fs::read(path) {
        Ok(bytes) => decode(bytes),
        Err(error) => Err(with_code("cannot read", &error)),
    }
}

pub struct Input {
    pub path: String,
    pub output: String,
    pub text: Decoded,
}

pub struct Formatted {
    pub findings: Vec<Finding>,
    pub failed: bool,
    pub only_generated: bool,
}

pub fn format_inputs(inputs: impl ExactSizeIterator<Item = Input>, mode: Mode) -> Formatted {
    let count = inputs.len();
    let mut findings = Vec::new();
    let mut failed = false;
    let mut generated = 0;
    for input in inputs {
        let result = format_text(&input.path, input.text, mode);
        failed |= result.parse_error;
        generated += usize::from(result.generated);
        findings.extend(result.findings.into_iter().map(|finding| Finding {
            path: input.output.clone(),
            ..finding
        }));
    }

    findings.sort_by(compare_findings);
    Formatted {
        findings,
        failed,
        only_generated: generated > 0 && generated == count,
    }
}
