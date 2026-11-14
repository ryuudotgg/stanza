use std::cmp::Ordering;

use serde::ser::{Serialize, SerializeStruct, Serializer};

use super::rules::Rule;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub line: usize,
    pub col: usize,
    pub rule: Rule,
    pub message: String,
    pub fixable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spot {
    pub offset: usize,
    pub rule: Rule,
    pub message: String,
}

pub fn compare_in_file(left: &Finding, right: &Finding) -> Ordering {
    left.line
        .cmp(&right.line)
        .then(left.col.cmp(&right.col))
        .then_with(|| left.rule.id().cmp(right.rule.id()))
}

#[derive(Debug)]
pub struct FileFindings {
    pub path: String,
    pub findings: Vec<Finding>,
}

impl FileFindings {
    pub fn located(&self) -> impl Iterator<Item = Located<'_>> {
        self.findings.iter().map(|finding| Located {
            path: &self.path,
            finding,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Located<'a> {
    pub path: &'a str,
    pub finding: &'a Finding,
}

impl Serialize for Located<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let finding = self.finding;
        let mut fields = serializer.serialize_struct("Finding", 6)?;
        fields.serialize_field("path", self.path)?;
        fields.serialize_field("line", &finding.line)?;
        fields.serialize_field("col", &finding.col)?;
        fields.serialize_field("rule", &finding.rule)?;
        fields.serialize_field("message", &finding.message)?;
        fields.serialize_field("fixable", &finding.fixable)?;
        fields.end()
    }
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
