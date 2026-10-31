use super::directives::{Region, within};
use super::doc::{blank_lines, comment_index, finding, line_at};
use std::collections::HashSet;

use super::model::{Doc, StatementList};
use super::types::{Finding, Rule};

fn edge_edits(
    path: &str,
    doc: &Doc,
    list: &StatementList,
    regions: &[Region],
    deleted: &mut HashSet<usize>,
) -> Vec<Finding> {
    let (Some(open_line), Some(close_line)) = (list.open_line, list.close_line) else {
        return Vec::new();
    };

    if open_line == close_line {
        return Vec::new();
    }

    let to = comment_index(doc, list.end);

    let mut opened = open_line;
    let mut from = comment_index(doc, list.start + 1);
    while from < to && line_at(doc, doc.comments[from].start) == open_line {
        opened = line_at(doc, doc.comments[from].end - 1);
        from += 1;
    }

    let (first_comment, last_comment) = if from < to {
        (
            line_at(doc, doc.comments[from].start),
            line_at(doc, doc.comments[to - 1].end - 1),
        )
    } else {
        (close_line, opened)
    };

    let first = list
        .stmts
        .first()
        .map_or(close_line, |stmt| stmt.start_line)
        .min(first_comment);

    let last = list
        .stmts
        .last()
        .map_or(opened, |stmt| stmt.end_line)
        .max(last_comment);

    let mut findings = Vec::new();
    for (after, before, side, edge) in [
        (opened, first, "after {", list.stmts.first()),
        (last, close_line, "before }", list.stmts.last()),
    ] {
        if edge.is_some_and(|stmt| stmt.frozen) {
            continue;
        }

        for line in blank_lines(doc, after, before) {
            let start = doc.line_starts[line - 1];
            if deleted.contains(&line) || within(regions, start) {
                continue;
            }

            deleted.insert(line);
            findings.push(finding(
                path,
                doc,
                start,
                Rule::EdgeBlank,
                format!("no blank line right {side}"),
            ));
        }
    }

    findings
}

pub fn spacing(path: &str, doc: &Doc, lists: &[StatementList], regions: &[Region]) -> Vec<Finding> {
    let mut deleted = HashSet::new();
    lists
        .iter()
        .flat_map(|list| edge_edits(path, doc, list, regions, &mut deleted))
        .collect()
}
