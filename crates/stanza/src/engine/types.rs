use std::cmp::Ordering;

use serde::Serialize;

use crate::text::locale_compare;

use super::rules::Rule;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub path: String,
    pub line: usize,
    pub col: usize,
    pub rule: Rule,
    pub message: String,
    pub fixable: bool,
}

pub fn compare_findings(left: &Finding, right: &Finding) -> Ordering {
    locale_compare(&left.path, &right.path)
        .then(left.line.cmp(&right.line))
        .then(left.col.cmp(&right.col))
        .then_with(|| locale_compare(left.rule.id(), right.rule.id()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Braces {
    On,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Fix,
    Check,
}

pub struct Fixed {
    pub text: String,
    pub error: Option<String>,
}

pub struct FileResult {
    pub findings: Vec<Finding>,
    pub parse_error: bool,
    pub fixed: Option<Fixed>,
}

impl FileResult {
    pub fn unchanged(findings: Vec<Finding>) -> Self {
        FileResult {
            findings,
            parse_error: false,
            fixed: None,
        }
    }

    pub fn fixed(findings: Vec<Finding>, fixed: Fixed) -> Self {
        FileResult {
            findings,
            parse_error: false,
            fixed: Some(fixed),
        }
    }
}
