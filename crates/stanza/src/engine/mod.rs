pub mod attach;
pub mod directives;
pub mod doc;
pub mod edits;
pub mod gaps;
pub mod model;
pub mod rules;
pub mod types;
pub mod widths;

use std::collections::HashMap;

use oxc_allocator::Allocator;

use crate::languages::FALLBACK_LAYOUT;
use crate::languages::javascript::braces::brace_edits;
use crate::languages::javascript::parse::{parse, rejection};
use crate::languages::javascript::scan::{Scan, comments, scan};

use doc::{document, parse_finding, positions};
use edits::{OffsetBack, apply_lines, apply_offsets};
use gaps::{Spacing, spacing};
use model::Doc;
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

struct Recorded {
    finding: Finding,
    offset: usize,
}

struct Trail<'i> {
    input: &'i Doc<'i>,
    backs: Vec<OffsetBack>,
    findings: Vec<Recorded>,
}

impl Trail<'_> {
    fn back(&self, offset: usize) -> usize {
        self.backs
            .iter()
            .rev()
            .fold(offset, |offset, back| back.back(offset))
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

fn examined<R>(path: &str, text: &str, then: impl FnOnce(&Doc, &Scan, Option<String>) -> R) -> R {
    let allocator = Allocator::default();
    let parsed = parse(&allocator, path, text);
    let doc = document(text, comments(text, &parsed.program));
    let scanned = scan(&doc, &parsed.program, FALLBACK_LAYOUT);
    let error = parsed
        .diagnostics
        .first()
        .map(|error| error.message.to_string());
    then(&doc, &scanned, error)
}

enum Stage {
    Unbraced(String),
    Settled(Vec<Finding>, Option<Fixed>),
}

struct Run<'i, 'a> {
    path: &'i str,
    keep_braces: bool,
    input: Input<'i, 'a>,
    trail: Option<Trail<'i>>,
}

impl Run<'_, '_> {
    fn stage(&mut self, doc: &Doc, scanned: &Scan, error: Option<String>) -> Stage {
        if !self.keep_braces {
            let pass = brace_edits(doc, &scanned.blocks, true, FALLBACK_LAYOUT);
            if !pass.edits.is_empty() {
                if let Some(trail) = &mut self.trail {
                    trail.record_braces(self.path, &pass.starts);
                    trail.backs.push(OffsetBack::new(&pass.edits));
                }

                return Stage::Unbraced(apply_offsets(doc.text, &pass.edits));
            }
        }

        let all = |_, _| true;
        let Spacing { edits, findings } =
            spacing(self.path, doc, &scanned.lists, &scanned.frozen, &all);

        if let Some(mut trail) = self.trail.take() {
            let (fixable, held): (Vec<_>, Vec<_>) =
                findings.into_iter().partition(|item| item.fixable);

            trail.record(doc, fixable);
            trail.record(doc, held);
            return Stage::Settled(net_gaps(trail, self.input, doc, scanned), None);
        }

        let changed = doc.text != self.input.doc.text;
        let spaced = apply_lines(doc, &edits);
        if spaced == doc.text {
            let fixed = changed.then_some(Fixed {
                text: spaced,
                error,
            });

            return Stage::Settled(remaining(findings), fixed);
        }

        examined(self.path, &spaced, |doc, scanned, error| {
            let all = |_, _| true;
            let findings = spacing(self.path, doc, &scanned.lists, &scanned.frozen, &all).findings;
            let fixed = Fixed {
                text: doc.text.to_owned(),
                error,
            };

            Stage::Settled(remaining(findings), Some(fixed))
        })
    }
}

pub fn process_file(path: &str, text: &str, mode: Mode, keep_braces: bool) -> FileResult {
    let allocator = Allocator::default();
    let parsed = parse(&allocator, path, text);
    if let Some(rejected) = rejection(text, &parsed) {
        return FileResult {
            findings: vec![parse_finding(path, text, rejected.start, rejected.message)],
            parse_error: true,
            fixed: None,
        };
    }

    let doc = document(text, comments(text, &parsed.program));
    let scanned = scan(&doc, &parsed.program, FALLBACK_LAYOUT);
    let mut run = Run {
        path,
        keep_braces,
        input: Input {
            doc: &doc,
            scanned: &scanned,
        },
        trail: (mode == Mode::Check).then(|| Trail {
            input: &doc,
            backs: Vec::new(),
            findings: Vec::new(),
        }),
    };

    let mut stage = run.stage(&doc, &scanned, None);
    while let Stage::Unbraced(next) = stage {
        stage = examined(path, &next, |doc, scanned, error| {
            run.stage(doc, scanned, error)
        });
    }

    let Stage::Settled(mut findings, fixed) = stage else {
        unreachable!("the loop ends on a settled stage");
    };

    findings.sort_by(compare_findings);
    FileResult {
        findings,
        parse_error: false,
        fixed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step::format_text;

    const SPREAD: &str = "function f() {\n  const x = {\n    a,\n  };\n  log(x);\n\n\n  b();\n}\n";

    const SPACED: &str =
        "function f() {\n  const x = {\n    a,\n  };\n\n  log(x);\n\n\n  b();\n}\n";

    fn fixed(text: &str) -> Option<String> {
        format_text("a.ts", Ok(text.to_owned()), Mode::Fix, true).fixed
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
        let checked = process_file("a.ts", SPREAD, Mode::Check, true);
        let fixed = process_file("a.ts", SPREAD, Mode::Fix, true);
        assert!(checked.findings.iter().any(|finding| finding.fixable));
        assert!(fixed.findings.iter().all(|finding| !finding.fixable));
        assert!(fixed.fixed.is_some_and(|fixed| fixed.error.is_none()));
    }

    fn unbraced(path: &str, text: &str) -> String {
        let result = format_text(path, Ok(text.to_owned()), Mode::Fix, false);
        assert!(!result.parse_error, "{path} parses");
        result.fixed.unwrap_or_else(|| text.to_owned())
    }

    #[test]
    fn a_wide_character_after_a_chain_closer_settles_as_the_reference() {
        let text = "while (x) { if (a) { b(); } } école();\n";
        let checked = process_file("a.ts", text, Mode::Check, false);
        let columns: Vec<usize> = checked.findings.iter().map(|finding| finding.col).collect();

        assert_eq!(columns, [11, 20]);
        assert_eq!(unbraced("a.ts", text), "while (x) if (a) b(); école();\n");
    }

    #[test]
    fn check_positions_braces_on_the_original_text() {
        let text = "function f() {\n  if (a) {\n    if (b) {\n      g();\n    }\n  }\n}\n";
        let result = process_file("a.ts", text, Mode::Check, false);
        let braces: Vec<(usize, usize)> = result
            .findings
            .iter()
            .filter(|finding| finding.rule == rules::Rule::Braces)
            .map(|finding| (finding.line, finding.col))
            .collect();

        assert_eq!(braces, [(2, 10), (3, 12)]);
    }

    fn thread_cpu_ms() -> f64 {
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
            process_file("large.ts", text, mode, false);
        }

        for _ in 0..rounds {
            for (index, text) in texts.iter().enumerate() {
                let started = thread_cpu_ms();
                process_file("large.ts", text, mode, false);
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
    fn one_line_of_braced_bodies_stays_linear() {
        let line = |count: usize| format!("{}\n", "if(a){f();}".repeat(count));
        let sizes = [2_000, 4_000, 8_000, 16_000];
        for mode in [Mode::Fix, Mode::Check] {
            let runs = fastest(&sizes, 3, mode, line);
            assert!(growth(&runs) <= 1.5, "{mode:?} {runs:?}");
        }

        assert_eq!(unbraced("a.ts", &line(3)), "if(a)f();if(a)f();if(a)f();\n");
    }
}
