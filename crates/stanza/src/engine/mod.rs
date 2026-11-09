pub mod attach;
pub mod directives;
pub mod doc;
pub mod edits;
pub mod explain;
pub mod gaps;
pub mod hunks;
pub mod model;
pub mod rules;
pub mod types;
pub mod widths;

use std::collections::{HashMap, HashSet};

use oxc_allocator::Allocator;
use oxc_ast::ast::Statement;
use oxc_span::GetSpan;

use crate::languages::Layout;
use crate::languages::javascript::braces::{BracePass, Controlled, brace_edits};
use crate::languages::javascript::facts::Node;
use crate::languages::javascript::lists::List;
use crate::languages::javascript::parse::{parse, rejection};
use crate::languages::javascript::scan::{Scan, comments, scan};

use doc::{document, line_at, parse_finding, positions};
use edits::{LineShift, OffsetEdit, OffsetMap, apply_lines, apply_offsets};
use gaps::{Spacing, spacing};
use hunks::{Before, Changed, after_lines, after_offsets, touching};
use model::{Doc, LineEdits};
use rules::{Rule, Want};
use types::{FileResult, Finding, Fixed, Mode, compare_findings};

fn offset_of(doc: &Doc, line: usize, col: usize) -> usize {
    let start = doc.line_starts[line - 1];
    let mut units = 0;
    for (index, char) in doc.text[start..].char_indices() {
        if units >= col - 1 {
            return start + index;
        }

        units += char.len_utf16();
    }

    doc.text.len()
}

struct Respacing {
    origins: Vec<usize>,
    before_starts: Vec<usize>,
    before_lengths: Vec<usize>,
}

impl Respacing {
    fn new(before: &Doc, edits: &LineEdits) -> Self {
        let count = before.lines.len();
        let mut origins = Vec::with_capacity(count);
        for line in 1..=count {
            if !edits.delete_lines.contains(&line) {
                origins.push(line);
            }

            if edits.insert_after.contains(&line) {
                origins.push((line + 1).min(count));
            }
        }

        Respacing {
            origins,
            before_starts: before.line_starts.clone(),
            before_lengths: before.lines.iter().map(|line| line.len()).collect(),
        }
    }
}

struct LineMap {
    respacing: Respacing,
    after_starts: Vec<usize>,
}

impl LineMap {
    fn back(&self, offset: usize) -> usize {
        let Respacing {
            origins,
            before_starts,
            before_lengths,
        } = &self.respacing;

        let line = self.after_starts.partition_point(|&start| start <= offset);
        let origin = origins[line - 1];
        let column = (offset - self.after_starts[line - 1]).min(before_lengths[origin - 1]);
        before_starts[origin - 1] + column
    }
}

enum Back {
    Offsets(OffsetMap),
    Lines(LineMap),
}

struct Recorded {
    finding: Finding,
    offset: usize,
}

struct Trail<'i> {
    input: &'i Doc<'i>,
    backs: Vec<Back>,
    findings: Vec<Recorded>,
}

impl Trail<'_> {
    fn back(&self, offset: usize) -> usize {
        self.backs
            .iter()
            .rev()
            .fold(offset, |offset, back| match back {
                Back::Offsets(map) => map.back(offset),
                Back::Lines(map) => map.back(offset),
            })
    }

    fn record_at(&mut self, items: Vec<(usize, Finding)>) {
        let offsets: Vec<usize> = items.iter().map(|(offset, _)| self.back(*offset)).collect();
        let positions = positions(self.input, &offsets);
        for (((_, finding), offset), (line, col)) in items.into_iter().zip(offsets).zip(positions) {
            self.findings.push(Recorded {
                finding: Finding {
                    line,
                    col,
                    ..finding
                },
                offset,
            });
        }
    }

    fn record(&mut self, doc: &Doc, findings: Vec<Finding>) {
        let items = findings
            .into_iter()
            .map(|finding| (offset_of(doc, finding.line, finding.col), finding))
            .collect();

        self.record_at(items);
    }

    fn record_braces(&mut self, path: &str, starts: &[usize]) {
        let items = starts
            .iter()
            .map(|&start| {
                let finding = Finding {
                    path: path.to_owned(),
                    line: 0,
                    col: 0,
                    rule: Rule::Braces,
                    message: Rule::Braces.message(&[]),
                    fixable: Rule::Braces.fixable(),
                };

                (start, finding)
            })
            .collect();

        self.record_at(items);
    }
}

fn gap_blanks(doc: &Doc, scanned: &Scan, back: impl Fn(usize) -> usize) -> HashMap<usize, usize> {
    let mut blanks = HashMap::new();
    for list in &scanned.lists {
        for pair in list.stmts.windows(2) {
            let blank = doc::blank_lines(doc, pair[0].end_line, pair[1].start_line).count();
            blanks.insert(back(pair[1].start), blank);
        }
    }

    blanks
}

#[derive(Clone, Copy)]
struct Input<'i, 'a> {
    doc: &'i Doc<'i>,
    scanned: &'i Scan<'a>,
}

fn net_gaps(trail: Trail, input: Input, last: &Doc, last_scanned: &Scan) -> Vec<Finding> {
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for (index, item) in trail.findings.iter().enumerate() {
        if item.finding.rule.gap().is_some() {
            groups.entry(item.offset).or_default().push(index);
        }
    }

    if groups.values().all(|group| group.len() == 1) {
        return trail
            .findings
            .into_iter()
            .map(|item| item.finding)
            .collect();
    }

    let before = gap_blanks(input.doc, input.scanned, |offset| offset);
    let after = gap_blanks(last, last_scanned, |offset| trail.back(offset));
    let blanks = |blanks: &HashMap<usize, usize>, offset| blanks.get(&offset).copied().unwrap_or(0);
    let kept: Vec<bool> = trail
        .findings
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let Some(group) = groups.get(&item.offset).filter(|group| group.len() > 1) else {
                return true;
            };

            let change = blanks(&after, item.offset).cmp(&blanks(&before, item.offset));
            let want = if change.is_gt() {
                Want::AtLeastOne
            } else {
                Want::None
            };

            let last = group
                .iter()
                .rev()
                .find(|&&other| trail.findings[other].finding.rule.gap() == Some(want));

            change.is_ne() && last == Some(&index)
        })
        .collect();

    trail
        .findings
        .into_iter()
        .zip(kept)
        .filter_map(|(item, kept)| kept.then_some(item.finding))
        .collect()
}

fn remaining(findings: Vec<Finding>) -> Vec<Finding> {
    findings.into_iter().filter(|item| !item.fixable).collect()
}

fn statement_start(node: Node) -> usize {
    let Node::Statement(mut statement) = node else {
        return node.span().start as usize;
    };

    while let Statement::LabeledStatement(labeled) = statement {
        statement = &labeled.body;
    }

    statement.span().start as usize
}

fn touched_statements(
    doc: &Doc,
    lists: &[List],
    touches: &dyn Fn(usize, usize) -> bool,
) -> HashSet<usize> {
    let mut touched = HashSet::new();
    for list in lists {
        for pair in list.stmts.windows(2) {
            let (prev, next) = (&pair[0], &pair[1]);
            if !touches(prev.start_line, next.end_line) {
                continue;
            }

            let short_body_may_join = list.stmts.len() <= 3
                && doc::blank_lines(doc, prev.end_line, next.start_line).count() > 0;

            let stmts = if short_body_may_join {
                &list.stmts[..]
            } else {
                pair
            };

            touched.extend(stmts.iter().map(|stmt| statement_start(stmt.node)));
        }
    }

    touched
}

fn examined<R>(
    path: &str,
    text: &str,
    layout: Layout,
    then: impl FnOnce(&Doc, &Scan, Option<String>) -> R,
) -> R {
    let allocator = Allocator::default();
    let parsed = parse(&allocator, path, text);
    let doc = document(text, comments(text, &parsed.program));
    let scanned = scan(&doc, &parsed.program, layout);
    let error = parsed
        .diagnostics
        .first()
        .map(|error| error.message.to_string());
    then(&doc, &scanned, error)
}

enum Stage {
    Unbraced {
        text: String,
        before: Option<Before>,
        edits: Vec<OffsetEdit>,
    },
    Respaced {
        text: String,
        respacing: Option<Respacing>,
    },
    Settled(Vec<Finding>, Option<Fixed>),
}

struct Run<'i, 'a> {
    path: &'i str,
    mode: Mode,
    keep_braces: bool,
    layout: Layout,
    input: Input<'i, 'a>,
    trail: Option<Trail<'i>>,
    lines: Option<Changed>,
    collapse: bool,
}

impl Run<'_, '_> {
    fn brace_pass(&self, doc: &Doc, scanned: &Scan, collapse: bool) -> BracePass {
        let Some(lines) = &self.lines else {
            return brace_edits(doc, &scanned.blocks, collapse, self.layout);
        };

        let touches = touching(Some(lines));
        let touched = touched_statements(doc, &scanned.lists, &touches);
        let blocks: Vec<Controlled> = scanned
            .blocks
            .iter()
            .filter(|controlled| {
                let span = controlled.block.span;
                let owner = &controlled.owner;
                touches(
                    line_at(doc, span.start as usize),
                    line_at(doc, span.end as usize - 1),
                ) || touched.contains(scanned.chains.get(owner).unwrap_or(owner))
            })
            .copied()
            .collect();

        brace_edits(doc, &blocks, collapse, self.layout)
    }

    fn start(&mut self, doc: &Doc, scanned: &Scan) -> Stage {
        let pass = (!self.keep_braces).then(|| self.brace_pass(doc, scanned, self.collapse));
        self.braced(doc, scanned, None, pass)
    }

    fn unbraced(
        &mut self,
        doc: &Doc,
        scanned: &Scan,
        error: Option<String>,
        before: Option<Before>,
        edits: &[OffsetEdit],
    ) -> Stage {
        if let Some(before) = before {
            self.lines = after_offsets(&before, self.lines.take(), edits, doc);
        }

        let pass = self.brace_pass(doc, scanned, self.collapse);
        self.braced(doc, scanned, error, Some(pass))
    }

    fn respaced(
        &mut self,
        doc: &Doc,
        scanned: &Scan,
        error: Option<String>,
        respacing: Option<Respacing>,
    ) -> Stage {
        if let (Some(trail), Some(respacing)) = (&mut self.trail, respacing) {
            trail.backs.push(Back::Lines(LineMap {
                respacing,
                after_starts: doc.line_starts.clone(),
            }));
        }

        if self.lines.is_none() || self.keep_braces {
            return self.settle(doc, scanned, error, None);
        }

        let pass = self.brace_pass(doc, scanned, false);
        if pass.edits.is_empty() {
            return self.settle(doc, scanned, error, None);
        }

        self.braced(doc, scanned, error, Some(pass))
    }

    fn braced(
        &mut self,
        doc: &Doc,
        scanned: &Scan,
        error: Option<String>,
        pass: Option<BracePass>,
    ) -> Stage {
        let Some(pass) = pass.filter(|pass| !pass.edits.is_empty()) else {
            return self.spaced(doc, scanned, error);
        };

        if let Some(trail) = &mut self.trail {
            trail.record_braces(self.path, &pass.starts);
            trail.backs.push(Back::Offsets(OffsetMap::new(&pass.edits)));
        }

        Stage::Unbraced {
            text: apply_offsets(doc.text, &pass.edits),
            before: self.lines.is_some().then(|| Before::of(doc)),
            edits: pass.edits,
        }
    }

    fn spaced(&mut self, doc: &Doc, scanned: &Scan, error: Option<String>) -> Stage {
        let Spacing { edits, findings } = spacing(
            self.path,
            doc,
            &scanned.lists,
            &scanned.frozen,
            &touching(self.lines.as_ref()),
        );

        if let Some(trail) = &mut self.trail {
            let fixable = findings
                .iter()
                .filter(|item| item.fixable)
                .cloned()
                .collect();
            trail.record(doc, fixable);
        }

        if edits.is_empty() || (self.mode == Mode::Check && self.lines.is_none()) {
            return self.settle(doc, scanned, error, Some(findings));
        }

        let text = apply_lines(doc, &edits);
        if text == doc.text {
            return self.settle(doc, scanned, error, Some(findings));
        }

        if self.mode == Mode::Fix && self.lines.is_none() && std::ptr::eq(doc, self.input.doc) {
            return self.shifted(text, &edits, findings, error);
        }

        self.lines = after_lines(doc, self.lines.take(), &edits);
        Stage::Respaced {
            text,
            respacing: self.trail.is_some().then(|| Respacing::new(doc, &edits)),
        }
    }

    fn shifted(
        &self,
        text: String,
        edits: &LineEdits,
        findings: Vec<Finding>,
        error: Option<String>,
    ) -> Stage {
        let shift = LineShift::new(edits);
        let findings: Vec<Finding> = remaining(findings)
            .into_iter()
            .map(|finding| Finding {
                line: shift.forward(finding.line),
                ..finding
            })
            .collect();

        #[cfg(debug_assertions)]
        examined(self.path, &text, self.layout, |doc, scanned, reparsed| {
            assert_eq!(
                (&self.unfixed(doc, scanned), &reparsed),
                (&findings, &error),
                "{}: findings moved through blank line edits differ from a reparse",
                self.path
            );
        });

        Stage::Settled(findings, Some(Fixed { text, error }))
    }

    fn unfixed(&self, doc: &Doc, scanned: &Scan) -> Vec<Finding> {
        let touches = touching(self.lines.as_ref());
        remaining(spacing(self.path, doc, &scanned.lists, &scanned.frozen, &touches).findings)
    }

    fn settle(
        &mut self,
        doc: &Doc,
        scanned: &Scan,
        error: Option<String>,
        findings: Option<Vec<Finding>>,
    ) -> Stage {
        let remaining = match findings {
            Some(findings) => remaining(findings),
            None => self.unfixed(doc, scanned),
        };

        let Some(mut trail) = self.trail.take() else {
            let fixed = (doc.text != self.input.doc.text).then(|| Fixed {
                text: doc.text.to_owned(),
                error,
            });

            return Stage::Settled(remaining, fixed);
        };

        trail.record(doc, remaining);
        Stage::Settled(net_gaps(trail, self.input, doc, scanned), None)
    }
}

pub fn process_file(
    path: &str,
    text: &str,
    mode: Mode,
    keep_braces: bool,
    layout: Layout,
    lines: Option<Changed>,
) -> FileResult {
    processed(path, text, mode, keep_braces, layout, lines).0
}

fn processed(
    path: &str,
    text: &str,
    mode: Mode,
    keep_braces: bool,
    layout: Layout,
    lines: Option<Changed>,
) -> (FileResult, usize) {
    let allocator = Allocator::default();
    let parsed = parse(&allocator, path, text);
    if let Some(rejected) = rejection(text, &parsed) {
        let result = FileResult {
            findings: vec![parse_finding(path, text, rejected.start, rejected.message)],
            parse_error: true,
            fixed: None,
        };

        return (result, 0);
    }

    let doc = document(text, comments(text, &parsed.program));
    let scanned = scan(&doc, &parsed.program, layout);
    let mut run = Run {
        path,
        mode,
        keep_braces,
        layout,
        input: Input {
            doc: &doc,
            scanned: &scanned,
        },
        trail: (mode == Mode::Check).then(|| Trail {
            input: &doc,
            backs: Vec::new(),
            findings: Vec::new(),
        }),
        collapse: lines.is_none(),
        lines,
    };

    let mut respaces = 0;
    let mut stage = run.start(&doc, &scanned);
    let (mut findings, fixed) = loop {
        stage = match stage {
            Stage::Settled(findings, fixed) => break (findings, fixed),
            Stage::Unbraced {
                text,
                before,
                edits,
            } => examined(path, &text, layout, |doc, scanned, error| {
                run.unbraced(doc, scanned, error, before, &edits)
            }),
            Stage::Respaced { text, respacing } => {
                respaces += 1;
                examined(path, &text, layout, |doc, scanned, error| {
                    run.respaced(doc, scanned, error, respacing)
                })
            }
        };
    };

    findings.sort_by(compare_findings);
    let result = FileResult {
        findings,
        parse_error: false,
        fixed,
    };

    (result, respaces)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::engine::types::Braces;
    use crate::languages::FALLBACK_LAYOUT;
    use crate::step::{Unread, format_text};

    const SPREAD: &str = "function f() {\n  const x = {\n    a,\n  };\n  log(x);\n\n\n  b();\n}\n";

    const SPACED: &str =
        "function f() {\n  const x = {\n    a,\n  };\n\n  log(x);\n\n\n  b();\n}\n";

    fn fixed(text: &str) -> Option<String> {
        format_text(
            "a.ts",
            &Ok(text.to_owned()),
            Mode::Fix,
            Some(Braces::Off),
            &mut Unread::default(),
            None,
        )
        .fixed
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

    fn fixed_with_respaces(
        text: &str,
        keep_braces: bool,
        lines: Option<Changed>,
    ) -> (FileResult, usize) {
        processed("a.ts", text, Mode::Fix, keep_braces, FALLBACK_LAYOUT, lines)
    }

    const WALLED: &str =
        "function f() {\n  a();\n  b();\n  c();\n  d();\n  e();\n  g();\n\n\n  h();\n\n}\n";

    #[test]
    fn a_blank_line_only_fix_settles_without_a_second_pass() {
        let (result, respaces) = fixed_with_respaces(WALLED, false, None);
        let walls: Vec<(usize, usize)> = result
            .findings
            .iter()
            .map(|finding| (finding.line, finding.col))
            .collect();

        assert_eq!(respaces, 0);
        assert_eq!(walls, [(2, 3)]);
        assert_eq!(
            result.fixed.map(|fixed| fixed.text).as_deref(),
            Some("function f() {\n  a();\n  b();\n  c();\n  d();\n  e();\n  g();\n\n\n  h();\n}\n")
        );
    }

    #[test]
    fn blank_line_fixes_around_comments_cases_and_decorators_settle_as_a_reparse() {
        let crlf = SPREAD.replace('\n', "\r\n");
        let shapes: [(&str, &[(usize, usize)]); 6] = [
            (
                "function f() { // note\n\n  a();\n  b();\n  c();\n  d();\n  e();\n  g();\n}\n",
                &[(2, 3)],
            ),
            (
                "switch (x) {\n  case 1:\n    a();\n    // falls through\n  case 2:\n    b();\n}\n",
                &[],
            ),
            ("const a = 1;\n@dec\nclass A {\n  m() {}\n}\nf();\n", &[]),
            (
                "function f() {\n  const x = {\n    a,\n  };\n  log(x);\n  a();\n  b();\n  c();\n  d();\n  e();\n}\n",
                &[(6, 3)],
            ),
            (SPREAD, &[]),
            (&crlf, &[]),
        ];

        for (shape, expected) in shapes {
            let (result, respaces) = fixed_with_respaces(shape, true, None);
            let positions: Vec<(usize, usize)> = result
                .findings
                .iter()
                .map(|finding| (finding.line, finding.col))
                .collect();

            assert_eq!(respaces, 0, "{shape:?}");
            assert!(result.fixed.is_some(), "{shape:?}");
            assert_eq!(positions, expected, "{shape:?}");
        }
    }

    #[test]
    fn a_fix_that_changes_braces_keeps_the_second_pass() {
        let text = "function f() {\n  if (a) {\n    b();\n  }\n  log(x);\n\n\n  c();\n}\n";
        let (result, respaces) = fixed_with_respaces(text, false, None);
        assert_eq!(respaces, 1);
        assert_eq!(
            result.fixed.map(|fixed| fixed.text).as_deref(),
            Some("function f() {\n  if (a) b();\n  log(x);\n  c();\n}\n")
        );
    }

    #[test]
    fn a_hunks_fix_keeps_the_second_pass() {
        for keep_braces in [true, false] {
            let changed = Changed {
                lines: (1..=12).collect(),
                ..Changed::default()
            };

            let (result, respaces) = fixed_with_respaces(WALLED, keep_braces, Some(changed));
            assert_eq!(respaces, 1, "keep braces {keep_braces}");
            assert!(result.fixed.is_some(), "keep braces {keep_braces}");
        }
    }

    #[test]
    fn check_reports_what_fix_removes() {
        let checked = process_file("a.ts", SPREAD, Mode::Check, true, FALLBACK_LAYOUT, None);
        let fixed = process_file("a.ts", SPREAD, Mode::Fix, true, FALLBACK_LAYOUT, None);
        assert!(checked.findings.iter().any(|finding| finding.fixable));
        assert!(fixed.findings.iter().all(|finding| !finding.fixable));
        assert!(fixed.fixed.is_some_and(|fixed| fixed.error.is_none()));
    }

    fn unbraced(path: &str, text: &str) -> String {
        let result = format_text(
            path,
            &Ok(text.to_owned()),
            Mode::Fix,
            Some(Braces::On),
            &mut Unread::default(),
            None,
        );
        assert!(!result.parse_error, "{path} parses");
        result.fixed.unwrap_or_else(|| text.to_owned())
    }

    #[test]
    fn a_wide_character_after_a_chain_closer_settles_as_the_reference() {
        let text = "while (x) { if (a) { b(); } } école();\n";
        let checked = process_file("a.ts", text, Mode::Check, false, FALLBACK_LAYOUT, None);
        let columns: Vec<usize> = checked.findings.iter().map(|finding| finding.col).collect();

        assert_eq!(columns, [11, 20]);
        assert_eq!(unbraced("a.ts", text), "while (x) if (a) b(); école();\n");
    }

    #[test]
    fn check_positions_braces_on_the_original_text() {
        let text = "function f() {\n  if (a) {\n    if (b) {\n      g();\n    }\n  }\n}\n";
        let result = process_file("a.ts", text, Mode::Check, false, FALLBACK_LAYOUT, None);
        let braces: Vec<(usize, usize)> = result
            .findings
            .iter()
            .filter(|finding| finding.rule == rules::Rule::Braces)
            .map(|finding| (finding.line, finding.col))
            .collect();

        assert_eq!(braces, [(2, 10), (3, 12)]);
    }

    fn guards(count: usize, split: bool) -> String {
        (0..count)
            .map(|index| {
                let gap = if split && index == 3 { "\n" } else { "" };
                format!("{gap}  if (status === {}) return \"{index}\";", 401 + index)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn wall_body(statements: &str) -> String {
        format!("function run(status, rows) {{\n{statements}\n}}\n")
    }

    fn scoped(source: &str, mode: Mode, lines: &[usize]) -> crate::step::StepResult {
        let changed = Changed {
            lines: lines.iter().copied().collect(),
            ..Changed::default()
        };

        let result = format_text(
            "wall.ts",
            &Ok(source.to_owned()),
            mode,
            Some(Braces::Off),
            &mut Unread::default(),
            Some(changed),
        );

        assert!(!result.parse_error);
        result
    }

    #[test]
    fn hunk_walls_respect_retained_blank_lines_outside_the_changed_scope() {
        let calls = "  console.log(0);\n  console.log(1);";
        let source = wall_body(&format!("{}\n  return \"z\";\n{calls}", guards(6, true)));
        for mode in [Mode::Check, Mode::Fix] {
            let result = scoped(&source, mode, &[11, 12]);
            let walls: Vec<(usize, bool)> = result
                .findings
                .iter()
                .filter(|finding| finding.rule == Rule::Wall)
                .map(|finding| (finding.line, finding.fixable))
                .collect();

            assert_eq!(walls, [(6, false)], "{mode:?}");
            assert_eq!(result.fixed, None, "{mode:?}");
        }
    }

    #[test]
    fn hunk_walls_recognize_a_forced_group_with_untouched_closed_gaps() {
        for final_blank in ["", "\n"] {
            let source = wall_body(&format!(
                "{}\n{final_blank}  return \"z\";",
                guards(6, false)
            ));
            for mode in [Mode::Check, Mode::Fix] {
                let result = scoped(&source, mode, &[4]);
                assert_eq!(result.findings, [], "{mode:?} {final_blank:?}");
                assert_eq!(result.fixed, None, "{mode:?} {final_blank:?}");
            }
        }
    }

    pub(crate) fn thread_cpu_ms() -> f64 {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };

        let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
        assert_eq!(status, 0, "the thread clock reads");
        time.tv_sec as f64 * 1_000.0 + time.tv_nsec as f64 / 1_000_000.0
    }

    fn fastest(
        sizes: &[usize],
        rounds: usize,
        mode: Mode,
        text: impl Fn(usize) -> String,
    ) -> Vec<(usize, f64)> {
        let texts: Vec<String> = sizes.iter().map(|&size| text(size)).collect();
        let mut best = vec![f64::INFINITY; sizes.len()];
        for text in &texts {
            process_file("large.ts", text, mode, false, FALLBACK_LAYOUT, None);
        }

        for _ in 0..rounds {
            for (index, text) in texts.iter().enumerate() {
                let started = thread_cpu_ms();
                process_file("large.ts", text, mode, false, FALLBACK_LAYOUT, None);
                best[index] = best[index].min(thread_cpu_ms() - started);
            }
        }

        sizes.iter().copied().zip(best).collect()
    }

    fn growth(samples: &[(usize, f64)]) -> f64 {
        let points: Vec<(f64, f64)> = samples
            .iter()
            .map(|&(size, ms)| ((size as f64).ln(), ms.ln()))
            .collect();

        let count = points.len() as f64;
        let mean_x = points.iter().map(|point| point.0).sum::<f64>() / count;
        let mean_y = points.iter().map(|point| point.1).sum::<f64>() / count;
        let covariance: f64 = points
            .iter()
            .map(|(x, y)| (x - mean_x) * (y - mean_y))
            .sum();

        let variance: f64 = points.iter().map(|(x, _)| (x - mean_x).powi(2)).sum();
        covariance / variance
    }

    fn body(statement: &str, count: usize) -> String {
        format!("function f() {{\n{}}}\n", statement.repeat(count))
    }

    #[test]
    fn fixing_thousands_of_braced_bodies_stays_linear() {
        let braced = "  if (a) {\n    f();\n  }\n";
        let plain = "  if (a) f();\n";
        let braced_runs = fastest(&[1_000, 2_000, 4_000, 8_000], 4, Mode::Fix, |count| {
            body(braced, count)
        });
        let plain_runs = fastest(&[8_000], 4, Mode::Fix, |count| body(plain, count));
        let largest = braced_runs[braced_runs.len() - 1].1;

        assert_eq!(
            unbraced("large.ts", &body(braced, 8_000)),
            unbraced("large.ts", &body(plain, 8_000))
        );
        assert!(
            largest <= plain_runs[0].1 * 10.0,
            "{largest} ms against {plain_runs:?}"
        );
        assert!(growth(&braced_runs) <= 1.5, "{braced_runs:?}");
    }

    #[test]
    fn member_joins_stay_linear_in_chain_depth() {
        let (reads, joins) = std::thread::Builder::new()
            .stack_size(crate::pool::STACK_BYTES)
            .spawn(|| {
                let chain = |depth: usize| format!("a{}", ".b".repeat(depth));
                let source = |write: &str, read: &str| {
                    format!(
                        "function f() {{\n  {write} = 1;\n  if ({read}) run();\n  done();\n  done();\n}}\n"
                    )
                };

                let depths = [1_000, 2_000, 4_000, 8_000];
                let reads = fastest(&depths, 7, Mode::Check, |depth| {
                    source("a.x", &chain(depth))
                });
                let joins = fastest(&depths, 7, Mode::Check, |depth| {
                    source(&chain(depth), &chain(depth * 2))
                });
                (reads, joins)
            })
            .unwrap()
            .join()
            .unwrap();

        assert!(growth(&reads) <= 1.5, "{reads:?}");
        assert!(growth(&joins) <= 1.5, "{joins:?}");
    }

    #[test]
    fn one_line_of_braced_bodies_stays_linear() {
        let line = |count: usize| format!("{}\n", "if(a){f();}".repeat(count));
        let sizes = [2_000, 4_000, 8_000, 16_000];
        for mode in [Mode::Fix, Mode::Check] {
            let runs = fastest(&sizes, 3, mode, line);
            assert!(growth(&runs) <= 1.5, "{mode:?} {runs:?}");
        }

        assert_eq!(unbraced("a.ts", &line(3)), "if(a)f();if(a)f();if(a)f();\n");
    }

    #[test]
    fn a_header_comment_over_thousands_of_statements_stays_linear() {
        let flat = |count: usize| format!("// header\n{}", "f();\n".repeat(count));
        let nested =
            |count: usize| format!("// header\nfunction g() {{\n{}}}\n", "f();\n".repeat(count));

        let shapes: [&dyn Fn(usize) -> String; 2] = [&flat, &nested];
        for (shape, text) in shapes.into_iter().enumerate() {
            for mode in [Mode::Check, Mode::Fix] {
                let runs = fastest(&[4_000, 16_000], 3, mode, text);
                let ratio = runs[1].1 / runs[0].1;
                assert!(ratio < 5.0, "shape {shape} {mode:?} {runs:?}");
            }
        }
    }
}
