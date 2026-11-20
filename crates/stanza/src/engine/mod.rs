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

#[cfg(test)]
mod wall_fixtures;

use std::cell::{Cell, OnceCell};
use std::collections::HashSet;

use crate::files::Changed;
use crate::languages::Layout;
use crate::languages::javascript::braces::{BracePass, Controlled, brace_edits};
use crate::languages::javascript::facts::Node;
use crate::languages::javascript::lists::List;
use crate::languages::javascript::scan::{Scan, examine, examine_source};

use doc::{line_at, parse_finding, placed};
use edits::{LineShift, OffsetEdit, OffsetMap, apply_lines, apply_offsets, back_through};
use gaps::{Spacing, Touches, short_body, spacing};
use hunks::{Before, after_offsets, touching};
use model::{Doc, LineEdits, Stmt};
use rules::Rule;
use types::{FileResult, Finding, Fixed, Mode, Spot, compare_in_file};

struct Trail {
    backs: Vec<OffsetMap>,
    findings: Vec<Spot>,
}

impl Trail {
    fn record(&mut self, spots: Vec<Spot>) {
        for spot in spots {
            let offset = back_through(&self.backs, spot.offset);
            self.findings.push(Spot { offset, ..spot });
        }
    }

    fn record_braces(&mut self, starts: &[usize]) {
        let spots = starts
            .iter()
            .map(|&start| Spot {
                offset: start,
                rule: Rule::Braces,
                message: Rule::Braces.message().to_owned(),
            })
            .collect();

        self.record(spots);
    }
}

fn remaining(findings: Vec<Spot>) -> Vec<Spot> {
    findings
        .into_iter()
        .filter(|item| !item.rule.fixable())
        .collect()
}

struct Unbraced {
    text: String,
    map: OffsetMap,
    removed: Vec<usize>,
}

#[derive(Default)]
struct Scope {
    owners: HashSet<usize>,
    blocks: HashSet<usize>,
}

fn hunk_scope(
    doc: &Doc,
    lists: &[List],
    touches: Touches,
    unbrace: impl FnOnce() -> Unbraced,
    compact_starts: impl FnOnce(&str) -> HashSet<usize>,
) -> Scope {
    let unbrace = Cell::new(Some(unbrace));
    let unbraced = OnceCell::new();
    let preview = || unbraced.get_or_init(|| unbrace.take().expect("unbraced once")());
    let compact_starts = Cell::new(Some(compact_starts));
    let reparsed = OnceCell::new();
    let compact = |stmt: &Stmt<Node>| {
        if stmt.facts.compact {
            return true;
        }

        let preview = preview();
        let line_start = doc.line_starts[stmt.start_line - 1];
        if !preview
            .map
            .edits_between(line_start.saturating_sub(1), stmt.end)
        {
            return false;
        }

        reparsed
            .get_or_init(|| compact_starts.take().expect("reparsed once")(&preview.text))
            .contains(&preview.map.forward(stmt.start))
    };

    let mut scope = Scope::default();
    for list in lists {
        for pair in list.stmts.windows(2) {
            let (prev, next) = (&pair[0], &pair[1]);
            if !touches(prev.start_line, next.end_line) {
                continue;
            }

            if !short_body(list, compact) {
                scope
                    .owners
                    .extend(pair.iter().map(|stmt| stmt.node.anchor()));
                continue;
            }

            scope
                .owners
                .extend(list.stmts.iter().map(|stmt| stmt.node.anchor()));

            if list.stmts.iter().any(|stmt| !stmt.facts.compact) {
                let (first, last) = (&list.stmts[0], &list.stmts[list.stmts.len() - 1]);
                let removed = &preview().removed;
                let from = removed.partition_point(|&start| start < first.start);
                let to = removed.partition_point(|&start| start < last.end);
                scope.blocks.extend(&removed[from..to]);
            }

            break;
        }
    }

    scope
}

enum Stage {
    Unbraced {
        text: String,
        before: Option<Before>,
        edits: Vec<OffsetEdit>,
    },
    Settled(Vec<Finding>, Option<Fixed>),
}

struct Run<'i> {
    path: &'i str,
    mode: Mode,
    keep_braces: bool,
    layout: Layout,
    input: &'i Doc<'i>,
    trail: Option<Trail>,
    lines: Option<Changed>,
}

impl Run<'_> {
    fn brace_pass(&self, doc: &Doc, scanned: &Scan) -> BracePass {
        let Some(lines) = &self.lines else {
            return brace_edits(doc, &scanned.eligible, self.layout);
        };

        let touches = touching(Some(lines));
        let scope = hunk_scope(
            doc,
            &scanned.lists,
            &touches,
            || self.unbraced_whole(doc, scanned),
            |text| self.compact_starts(text),
        );
        let blocks: Vec<Controlled> = scanned
            .eligible
            .iter()
            .filter(|controlled| {
                let span = controlled.block.span;
                let owner = &controlled.owner;
                touches(
                    line_at(doc, span.start as usize),
                    line_at(doc, span.end as usize - 1),
                ) || scope
                    .owners
                    .contains(scanned.chains.get(owner).unwrap_or(owner))
                    || scope.blocks.contains(&(span.start as usize))
            })
            .copied()
            .collect();

        brace_edits(doc, &blocks, self.layout)
    }

    fn unbraced_whole(&self, doc: &Doc, scanned: &Scan) -> Unbraced {
        let pass = brace_edits(doc, &scanned.eligible, self.layout);
        let mut removed = pass.starts;
        removed.sort_unstable();
        Unbraced {
            text: apply_offsets(doc.text, &pass.edits),
            map: OffsetMap::new(&pass.edits),
            removed,
        }
    }

    fn compact_starts(&self, text: &str) -> HashSet<usize> {
        examine(self.path, text, self.layout, |_, unbraced, _| {
            unbraced
                .lists
                .iter()
                .flat_map(|list| &list.stmts)
                .filter(|stmt| stmt.facts.compact)
                .map(|stmt| stmt.start)
                .collect()
        })
    }

    fn start(&mut self, doc: &Doc, scanned: &Scan) -> Stage {
        let pass = (!self.keep_braces).then(|| self.brace_pass(doc, scanned));
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
            self.lines = self
                .lines
                .take()
                .map(|lines| after_offsets(&before, lines, edits, doc));
        }

        let pass = self.brace_pass(doc, scanned);
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
            trail.record_braces(&pass.starts);
            trail.backs.push(OffsetMap::new(&pass.edits));
        }

        Stage::Unbraced {
            text: apply_offsets(doc.text, &pass.edits),
            before: self.lines.is_some().then(|| Before::of(doc)),
            edits: pass.edits,
        }
    }

    fn spaced(&mut self, doc: &Doc, scanned: &Scan, error: Option<String>) -> Stage {
        let Spacing { edits, findings } = spacing(
            doc,
            &scanned.lists,
            &scanned.frozen,
            &touching(self.lines.as_ref()),
        );

        if let Some(trail) = &mut self.trail {
            let fixable = findings
                .iter()
                .filter(|item| item.rule.fixable())
                .cloned()
                .collect();
            trail.record(fixable);
        }

        if edits.is_empty() || self.mode == Mode::Check {
            return self.settle(doc, error, findings);
        }

        let text = apply_lines(doc, &edits);
        if text == doc.text {
            return self.settle(doc, error, findings);
        }

        Self::shifted(doc, text, &edits, findings, error)
    }

    fn shifted(
        doc: &Doc,
        text: String,
        edits: &LineEdits,
        findings: Vec<Spot>,
        error: Option<String>,
    ) -> Stage {
        let shift = LineShift::new(edits);
        let findings: Vec<Finding> = placed(doc, remaining(findings))
            .into_iter()
            .map(|finding| Finding {
                line: shift.forward(finding.line),
                ..finding
            })
            .collect();

        Stage::Settled(findings, Some(Fixed { text, error }))
    }

    #[cfg(debug_assertions)]
    fn unfixed(&self, doc: &Doc, scanned: &Scan) -> Vec<Spot> {
        let touches = touching(self.lines.as_ref());
        remaining(spacing(doc, &scanned.lists, &scanned.frozen, &touches).findings)
    }

    fn settle(&mut self, doc: &Doc, error: Option<String>, findings: Vec<Spot>) -> Stage {
        let remaining = remaining(findings);
        let Some(mut trail) = self.trail.take() else {
            let fixed = (doc.text != self.input.text).then(|| Fixed {
                text: doc.text.to_owned(),
                error,
            });

            return Stage::Settled(placed(doc, remaining), fixed);
        };

        trail.record(remaining);
        debug_assert!(
            {
                let mut offsets = HashSet::new();
                trail
                    .findings
                    .iter()
                    .filter(|item| item.rule.gap().is_some())
                    .all(|item| offsets.insert(item.offset))
            },
            "two gap findings share an offset"
        );

        Stage::Settled(placed(self.input, trail.findings), None)
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
    let examined = examine_source(path, text, layout, |doc, scanned| {
        settle(path, doc, scanned, mode, keep_braces, layout, lines)
    });

    examined.unwrap_or_else(|rejected| FileResult {
        findings: vec![parse_finding(text, rejected.start, rejected.message)],
        parse_error: true,
        fixed: None,
    })
}

fn settle(
    path: &str,
    doc: &Doc,
    scanned: &Scan,
    mode: Mode,
    keep_braces: bool,
    layout: Layout,
    lines: Option<Changed>,
) -> FileResult {
    let mut run = Run {
        path,
        mode,
        keep_braces,
        layout,
        input: doc,
        trail: (mode == Mode::Check).then(|| Trail {
            backs: Vec::new(),
            findings: Vec::new(),
        }),
        lines,
    };

    let mut stage = run.start(doc, scanned);
    let (mut findings, fixed) = loop {
        stage = match stage {
            Stage::Settled(findings, fixed) => break (findings, fixed),
            Stage::Unbraced {
                text,
                before,
                edits,
            } => examine(path, &text, layout, |doc, scanned, error| {
                run.unbraced(doc, scanned, error, before, &edits)
            }),
        };
    };

    #[cfg(debug_assertions)]
    if let Some(fixed) = &fixed {
        examine(path, &fixed.text, layout, |doc, scanned, reparsed| {
            assert_eq!(
                reparsed, fixed.error,
                "{path}: the fixed text reparses differently"
            );
            if run.lines.is_none() {
                assert_eq!(
                    placed(doc, run.unfixed(doc, scanned)),
                    findings,
                    "{path}: findings on the fixed text differ from a reparse"
                );
            }
        });
    }

    findings.sort_by(compare_in_file);
    FileResult {
        findings,
        parse_error: false,
        fixed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::types::Braces;
    use crate::engine::wall_fixtures::{calls, guards, wall_body};
    use crate::languages::FALLBACK_LAYOUT;
    use crate::step::{Unread, format_text};
    use crate::testing::timing::{
        LINEAR_GROWTH_CAP, assert_bounded, cpu_ms, on_deep_stack, ratio, slope,
    };

    const SPREAD: &str = "function f() {\n  const x = {\n    a,\n  };\n  log(x);\n\n\n  b();\n}\n";

    const SPACED: &str =
        "function f() {\n  const x = {\n    a,\n  };\n\n  log(x);\n\n\n  b();\n}\n";

    fn fixed(text: &str) -> Option<String> {
        let result = format_text(
            "a.ts",
            text,
            Mode::Fix,
            Some(Braces::Off),
            &mut Unread::default(),
            None,
        );

        result.fixed.map(|fixed| {
            assert_eq!(fixed.error, None);
            fixed.text
        })
    }

    #[test]
    fn fix_writes_the_spacing_edits() {
        assert_eq!(fixed(SPREAD).as_deref(), Some(SPACED));
    }

    #[test]
    fn fix_spaces_past_comment_interior_blanks() {
        assert_eq!(
            fixed(
                "function f() {\n  const x = {\n    a,\n  };\n  /*\n   note\n\n   more\n  */\n  run(x);\n  done();\n  done();\n}\n"
            )
            .as_deref(),
            Some(
                "function f() {\n  const x = {\n    a,\n  };\n\n  /*\n   note\n\n   more\n  */\n  run(x);\n  done();\n  done();\n}\n"
            )
        );
        assert_eq!(
            fixed(
                "function f() {\n  const x = a;\n\n  /* note\n\n     more */\n  if (x) return;\n  done();\n  done();\n}\n"
            )
            .as_deref(),
            Some(
                "function f() {\n  const x = a;\n  /* note\n\n     more */\n  if (x) return;\n\n  done();\n  done();\n}\n"
            )
        );
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

    fn fixed_once(text: &str, keep_braces: bool, lines: Option<Changed>) -> FileResult {
        process_file("a.ts", text, Mode::Fix, keep_braces, FALLBACK_LAYOUT, lines)
    }

    const WALLED: &str =
        "function f() {\n  a();\n  b();\n  c();\n  d();\n  e();\n  g();\n\n\n  h();\n\n}\n";

    #[test]
    fn a_blank_line_only_fix_places_the_wall_on_the_fixed_text() {
        let result = fixed_once(WALLED, false, None);
        let walls: Vec<(usize, usize)> = result
            .findings
            .iter()
            .map(|finding| (finding.line, finding.col))
            .collect();

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
            let result = fixed_once(shape, true, None);
            let positions: Vec<(usize, usize)> = result
                .findings
                .iter()
                .map(|finding| (finding.line, finding.col))
                .collect();

            assert!(result.fixed.is_some(), "{shape:?}");
            assert_eq!(positions, expected, "{shape:?}");
        }
    }

    #[test]
    fn a_fix_removes_braces_before_it_spaces() {
        let text = "function f() {\n  if (a) {\n    b();\n  }\n  log(x);\n\n\n  c();\n}\n";
        assert_eq!(
            fixed_once(text, false, None)
                .fixed
                .map(|fixed| fixed.text)
                .as_deref(),
            Some("function f() {\n  if (a) b();\n  log(x);\n  c();\n}\n")
        );
    }

    #[test]
    fn a_hunk_fix_over_every_line_matches_a_whole_file_fix() {
        for keep_braces in [true, false] {
            let changed = Changed {
                lines: (1..=12).collect(),
                ..Changed::default()
            };

            let whole = fixed_once(WALLED, keep_braces, None);
            let hunks = fixed_once(WALLED, keep_braces, Some(changed));
            assert_eq!(hunks.findings, whole.findings, "keep braces {keep_braces}");
            assert_eq!(
                hunks.fixed.map(|fixed| fixed.text),
                whole.fixed.map(|fixed| fixed.text),
                "keep braces {keep_braces}"
            );
        }
    }

    #[test]
    fn a_hunk_places_a_wall_where_its_mode_reads_it() {
        let text = "function f() {\n\n  a();\n  b();\n  c();\n  d();\n  e();\n  g();\n  h();\n}\n";
        let changed = || {
            Some(Changed {
                lines: [3].into(),
                ..Changed::default()
            })
        };

        let positions = |result: &FileResult| -> Vec<(Rule, usize, usize)> {
            result
                .findings
                .iter()
                .map(|finding| (finding.rule, finding.line, finding.col))
                .collect()
        };

        let checked = process_file("a.ts", text, Mode::Check, true, FALLBACK_LAYOUT, changed());
        let fixed = fixed_once(text, true, changed());
        assert_eq!(
            positions(&checked),
            [(Rule::EdgeBlank, 2, 1), (Rule::Wall, 3, 3)]
        );
        assert_eq!(positions(&fixed), [(Rule::Wall, 2, 3)]);
        assert_eq!(
            fixed.fixed.map(|fixed| fixed.text).as_deref(),
            Some("function f() {\n  a();\n  b();\n  c();\n  d();\n  e();\n  g();\n  h();\n}\n")
        );
    }

    #[test]
    fn a_hunk_widens_when_braces_earlier_on_a_line_make_its_statement_compact() {
        let long = "x".repeat(41);
        let text =
            format!("function f(){{ if (a) {{ b(); }} if ({long})\n  c();\n\n  changed();\n}}\n");
        let changed = || {
            Some(Changed {
                lines: [4].into(),
                ..Changed::default()
            })
        };

        let checked = process_file(
            "a.ts",
            &text,
            Mode::Check,
            false,
            FALLBACK_LAYOUT,
            changed(),
        );
        let found: Vec<(Rule, usize, usize)> = checked
            .findings
            .iter()
            .map(|finding| (finding.rule, finding.line, finding.col))
            .collect();

        assert_eq!(found, [(Rule::Braces, 1, 22), (Rule::ShortBody, 4, 3)]);
        assert_eq!(
            fixed_once(&text, false, changed())
                .fixed
                .map(|fixed| fixed.text),
            Some(format!(
                "function f(){{ if (a) b(); if ({long})\n  c();\n  changed();\n}}\n"
            ))
        );
    }

    #[test]
    fn a_touched_gap_with_no_blank_line_widens_to_a_short_body() {
        let text = "function f() {\n  if (a) {\n    first();\n  }\n  second();\n  changed();\n}\n";
        let changed = || {
            Some(Changed {
                lines: [6].into(),
                ..Changed::default()
            })
        };

        let checked = process_file("a.ts", text, Mode::Check, false, FALLBACK_LAYOUT, changed());
        let found: Vec<(Rule, usize, usize)> = checked
            .findings
            .iter()
            .map(|finding| (finding.rule, finding.line, finding.col))
            .collect();

        assert_eq!(found, [(Rule::Braces, 2, 10)]);
        assert_eq!(
            fixed_once(text, false, changed())
                .fixed
                .map(|fixed| fixed.text)
                .as_deref(),
            Some("function f() {\n  if (a) first();\n  second();\n  changed();\n}\n")
        );
    }

    #[test]
    fn check_reports_what_fix_removes() {
        let checked = process_file("a.ts", SPREAD, Mode::Check, true, FALLBACK_LAYOUT, None);
        let fixed = process_file("a.ts", SPREAD, Mode::Fix, true, FALLBACK_LAYOUT, None);
        assert!(
            checked
                .findings
                .iter()
                .any(|finding| finding.rule.fixable())
        );
        assert!(fixed.findings.iter().all(|finding| !finding.rule.fixable()));
        assert!(fixed.fixed.is_some_and(|fixed| fixed.error.is_none()));
    }

    fn unbraced(path: &str, text: &str) -> String {
        let result = format_text(
            path,
            text,
            Mode::Fix,
            Some(Braces::On),
            &mut Unread::default(),
            None,
        );
        assert!(!result.parse_error, "{path} parses");
        result.fixed.map_or_else(
            || text.to_owned(),
            |fixed| {
                assert_eq!(fixed.error, None, "{path} fix parses");
                fixed.text
            },
        )
    }

    #[test]
    fn a_wide_character_after_a_chain_closer_keeps_its_columns_and_unbraces() {
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

    #[test]
    fn a_hunk_inside_a_chain_of_same_line_closers_unbraces_in_one_fix() {
        let text = "function f() { while (x) { while (y)\n{\n  g(); } } }\n";
        let changed = || {
            Some(Changed {
                lines: [2].into(),
                ..Changed::default()
            })
        };

        let checked = process_file("a.ts", text, Mode::Check, false, FALLBACK_LAYOUT, changed());
        let braces: Vec<(usize, usize)> = checked
            .findings
            .iter()
            .filter(|finding| finding.rule == rules::Rule::Braces)
            .map(|finding| (finding.line, finding.col))
            .collect();

        assert_eq!(braces, [(1, 26), (2, 1)]);

        let fixed = process_file("a.ts", text, Mode::Fix, false, FALLBACK_LAYOUT, changed());
        assert_eq!(
            fixed.fixed.map(|fixed| fixed.text).as_deref(),
            Some("function f() { while (x) while (y) g(); }\n")
        );
    }

    fn scoped(source: &str, mode: Mode, lines: &[usize]) -> FileResult {
        let changed = Changed {
            lines: lines.iter().copied().collect(),
            ..Changed::default()
        };

        let result = format_text(
            "wall.ts",
            source,
            mode,
            Some(Braces::Off),
            &mut Unread::default(),
            Some(changed),
        );

        assert!(!result.parse_error);
        assert!(
            result
                .fixed
                .as_ref()
                .is_none_or(|fixed| fixed.error.is_none())
        );
        result
    }

    #[test]
    fn hunk_walls_respect_retained_blank_lines_outside_the_changed_scope() {
        let ret = vec!["  return \"z\";".to_owned()];
        let source = wall_body(&[guards(6, true), ret, calls(2)].concat());
        for mode in [Mode::Check, Mode::Fix] {
            let result = scoped(&source, mode, &[11, 12]);
            let walls: Vec<(usize, bool)> = result
                .findings
                .iter()
                .filter(|finding| finding.rule == Rule::Wall)
                .map(|finding| (finding.line, finding.rule.fixable()))
                .collect();

            assert_eq!(walls, [(6, false)], "{mode:?}");
            assert!(result.fixed.is_none(), "{mode:?}");
        }
    }

    #[test]
    fn hunk_walls_recognize_a_forced_group_with_untouched_closed_gaps() {
        for final_blank in ["", "\n"] {
            let ret = format!("{final_blank}  return \"z\";");
            let source = wall_body(&[guards(6, false), vec![ret]].concat());
            for mode in [Mode::Check, Mode::Fix] {
                let result = scoped(&source, mode, &[4]);
                assert_eq!(result.findings, [], "{mode:?} {final_blank:?}");
                assert!(result.fixed.is_none(), "{mode:?} {final_blank:?}");
            }
        }
    }

    fn processing(mode: Mode) -> impl FnMut(&String) -> f64 {
        move |text| {
            cpu_ms(|| {
                process_file("large.ts", text, mode, false, FALLBACK_LAYOUT, None);
            })
        }
    }

    fn body(statement: &str, count: usize) -> String {
        format!("function f() {{\n{}}}\n", statement.repeat(count))
    }

    #[test]
    fn fixing_thousands_of_braced_bodies_stays_linear() {
        let braced = "  if (a) {\n    f();\n  }\n";
        let plain = "  if (a) f();\n";
        assert_eq!(
            unbraced("large.ts", &body(braced, 8_000)),
            unbraced("large.ts", &body(plain, 8_000))
        );

        assert_bounded(
            "braced bodies",
            &[1_000, 2_000, 4_000, 8_000],
            4,
            slope(1.5),
            |count| body(braced, count),
            processing(Mode::Fix),
        );
        assert_bounded(
            "braced against plain bodies",
            &[plain, braced],
            4,
            ratio(10.0),
            |statement| body(statement, 8_000),
            processing(Mode::Fix),
        );
    }

    #[test]
    fn member_joins_stay_linear_in_chain_depth() {
        on_deep_stack(|| {
            let chain = |depth: usize| format!("a{}", ".b".repeat(depth));
            let source = |write: &str, read: &str| {
                format!(
                    "function f() {{\n  {write} = 1;\n  if ({read}) run();\n  done();\n  done();\n}}\n"
                )
            };

            let depths = [1_000, 2_000, 4_000, 8_000];
            assert_bounded(
                "reads",
                &depths,
                7,
                slope(1.5),
                |depth| source("a.x", &chain(depth)),
                processing(Mode::Check),
            );
            assert_bounded(
                "joins",
                &depths,
                7,
                slope(1.5),
                |depth| source(&chain(depth), &chain(depth * 2)),
                processing(Mode::Check),
            );
        });
    }

    #[test]
    fn one_line_of_braced_bodies_stays_linear() {
        let line = |count: usize| format!("{}\n", "if(a){f();}".repeat(count));
        let sizes = [2_000, 4_000, 8_000, 16_000];
        for mode in [Mode::Fix, Mode::Check] {
            assert_bounded(
                &format!("{mode:?}"),
                &sizes,
                3,
                slope(1.5),
                line,
                processing(mode),
            );
        }

        assert_eq!(unbraced("a.ts", &line(3)), "if(a)f();if(a)f();if(a)f();\n");
    }

    #[test]
    fn one_line_of_walls_stays_linear() {
        let line = |count: usize| {
            let walls: Vec<String> = (0..count)
                .map(|index| format!("function f{index}() {{ a(); b(); c(); d(); e(); f(); }}"))
                .collect();
            format!("{}\n", walls.join(" "))
        };

        for mode in [Mode::Check, Mode::Fix] {
            let walled = process_file("large.ts", &line(1_000), mode, false, FALLBACK_LAYOUT, None);
            assert_eq!(
                walled.findings.len(),
                1_001,
                "{mode:?}: a wall per body and one for the run of functions"
            );
            assert!(
                walled
                    .findings
                    .iter()
                    .all(|finding| finding.rule == Rule::Wall),
                "{mode:?}"
            );

            assert_bounded(
                &format!("{mode:?}"),
                &[1_000, 2_000, 4_000],
                5,
                slope(1.5),
                line,
                processing(mode),
            );
        }
    }

    fn stays_linear(text: fn(usize) -> String) {
        on_deep_stack(move || {
            for mode in [Mode::Check, Mode::Fix] {
                assert_bounded(
                    &format!("{mode:?}"),
                    &[2_000, 8_000],
                    5,
                    ratio(LINEAR_GROWTH_CAP),
                    text,
                    processing(mode),
                );
            }
        });
    }

    pub(super) fn compact_nesting(depth: usize) -> String {
        format!("{}f(){}\n", "if(a){".repeat(depth), "}".repeat(depth))
    }

    fn checked_braces(text: &str, lines: Option<Changed>) -> usize {
        process_file("large.ts", text, Mode::Check, false, FALLBACK_LAYOUT, lines)
            .findings
            .iter()
            .filter(|finding| finding.rule == rules::Rule::Braces)
            .count()
    }

    #[test]
    fn bodies_closed_on_one_line_settle_linearly_in_depth() {
        on_deep_stack(|| {
            let first_line = || {
                Some(Changed {
                    lines: [1].into(),
                    ..Changed::default()
                })
            };

            let spaced = |depth: usize| {
                format!(
                    "{}f();{} g();\n",
                    "while (x) { ".repeat(depth),
                    " }".repeat(depth)
                )
            };

            assert_eq!(checked_braces(&compact_nesting(400), None), 400);
            assert_eq!(checked_braces(&compact_nesting(400), first_line()), 400);

            let depths = [400, 1_600];
            assert_bounded(
                "compact whole-file",
                &depths,
                3,
                ratio(LINEAR_GROWTH_CAP),
                compact_nesting,
                processing(Mode::Check),
            );
            assert_bounded(
                "compact hunks",
                &depths,
                3,
                ratio(LINEAR_GROWTH_CAP),
                compact_nesting,
                |text| {
                    cpu_ms(|| {
                        std::hint::black_box(checked_braces(text, first_line()));
                    })
                },
            );
            assert_bounded(
                "spaced whole-file",
                &depths,
                3,
                ratio(LINEAR_GROWTH_CAP),
                spaced,
                processing(Mode::Check),
            );
        });
    }

    #[test]
    fn an_unbraced_else_if_chain_stays_linear_in_its_arms() {
        stays_linear(|count| {
            let arms: Vec<String> = (0..count).map(|arm| format!("if (a{arm}) f();")).collect();
            format!("{}\n", arms.join("\nelse "))
        });
    }

    #[test]
    fn a_let_run_read_by_an_if_stays_linear_in_its_length() {
        stays_linear(|count| {
            let lets: String = (1..=count)
                .map(|index| format!("  let a{index} = {index};\n"))
                .collect();
            let sum: Vec<String> = (1..=count).map(|index| format!("a{index}")).collect();
            format!(
                "function f() {{\n{lets}  if ({}) {{}}\n}}\n",
                sum.join(" + ")
            )
        });
    }

    #[test]
    fn a_header_comment_over_thousands_of_statements_stays_linear() {
        let flat = |count: usize| format!("// header\n{}", "f();\n".repeat(count));
        let nested =
            |count: usize| format!("// header\nfunction g() {{\n{}}}\n", "f();\n".repeat(count));

        let shapes: [&dyn Fn(usize) -> String; 2] = [&flat, &nested];
        for (shape, text) in shapes.into_iter().enumerate() {
            for mode in [Mode::Check, Mode::Fix] {
                assert_bounded(
                    &format!("shape {shape} {mode:?}"),
                    &[4_000, 8_000, 16_000],
                    5,
                    slope(1.5),
                    text,
                    processing(mode),
                );
            }
        }
    }
}
