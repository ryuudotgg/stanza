use crate::text::js_trim_start;

use super::directives::ignored;
use super::doc::{blank_lines, comment_index, line_at};
use super::model::{Doc, Item, Kind, Stmt};

fn falls_through(value: &str) -> bool {
    let value = js_trim_start(value).as_bytes();
    ["falls through", "fallthrough"].iter().any(|word| {
        value
            .get(..word.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(word.as_bytes()))
    })
}

fn fallthrough_end<N>(doc: &Doc, item: &Item<N>, next: Option<&Item<N>>, end: usize) -> usize {
    let Some(next) = next.filter(|next| item.facts.case_body && next.facts.kind == Kind::Case)
    else {
        return end;
    };

    let index = comment_index(doc, end);
    let Some(comment) = doc.comments.get(index) else {
        return end;
    };

    if doc
        .comments
        .get(index + 1)
        .is_some_and(|after| after.start < next.start)
    {
        return end;
    }

    let directly_between = line_at(doc, comment.start) == line_at(doc, end - 1) + 1
        && line_at(doc, comment.end - 1) + 1 == line_at(doc, next.start);

    if directly_between && falls_through(comment.value) {
        comment.end
    } else {
        end
    }
}

pub fn attach<N>(doc: &Doc, items: Vec<Item<N>>, opener: usize, limit: usize) -> Vec<Stmt<N>> {
    let mut stmts = Vec::with_capacity(items.len());
    let mut previous_end = opener;
    let mut items = items.into_iter().peekable();
    while let Some(item) = items.next() {
        let item_start_line = line_at(doc, item.start);
        let item_end_line = line_at(doc, item.end - 1);
        let to = comment_index(doc, item.start);
        let leading = &doc.comments[comment_index(doc, previous_end).min(to)..to];
        let lead_line = leading
            .first()
            .map_or(item_start_line, |comment| line_at(doc, comment.start));

        let detached = leading
            .iter()
            .map(|comment| line_at(doc, comment.end - 1))
            .zip(
                leading
                    .iter()
                    .skip(1)
                    .map(|comment| line_at(doc, comment.start))
                    .chain([item_start_line]),
            )
            .any(|(after, before)| blank_lines(doc, after, before).next().is_some());

        let mut end = item.end;
        for comment in &doc.comments[comment_index(doc, item.end)..] {
            if comment.start >= limit || line_at(doc, comment.start) != item_end_line {
                break;
            }

            end = end.max(comment.end);
        }

        if item.facts.kind == Kind::Case {
            end = fallthrough_end(doc, &item, items.peek(), end);
        }

        let fits_when_joined = item.facts.compact && item.facts.joined.is_some();

        previous_end = end;
        stmts.push(Stmt {
            node: item.node,
            start: item.start,
            end: item.end,
            facts: item.facts,
            frozen: ignored(doc, item.start),
            start_line: if detached { item_start_line } else { lead_line },
            end_line: line_at(doc, end - 1),
            multiline: item_start_line != item_end_line && !fits_when_joined,
            detached,
        });
    }

    stmts
}

#[cfg(test)]
mod tests {
    use crate::engine::process_file;
    use crate::engine::rules::Rule;
    use crate::engine::types::Mode;
    use crate::languages::FALLBACK_LAYOUT;
    use crate::languages::javascript::scan::scanned;

    fn lines(text: &str) -> Vec<(usize, usize, bool)> {
        scanned("a.ts", text, |_, scan| {
            scan.lists
                .last()
                .expect("a statement list")
                .stmts
                .iter()
                .map(|stmt| (stmt.start_line, stmt.end_line, stmt.detached))
                .collect()
        })
    }

    #[test]
    fn a_leading_comment_joins_unless_a_blank_line_detaches_it() {
        assert_eq!(
            lines("a();\n// note\nb();\n"),
            [(1, 1, false), (2, 3, false)]
        );
        assert_eq!(
            lines("a();\n// note\n\nb();\n"),
            [(1, 1, false), (4, 4, true)]
        );
    }

    #[test]
    fn comment_interior_blanks_leave_a_leading_comment_attached() {
        assert_eq!(
            lines("a();\n/* x\n\n y */\nb();\n"),
            [(1, 1, false), (2, 5, false)]
        );
        assert_eq!(
            lines("a();\n/* x\n\n y */\n\nb();\n"),
            [(1, 1, false), (6, 6, true)]
        );
        assert_eq!(
            lines("a();\n/* x\n\n y */\n// z\nb();\n"),
            [(1, 1, false), (2, 6, false)]
        );
        assert_eq!(
            lines("a();\n/* x */\n\n// z\nb();\n"),
            [(1, 1, false), (5, 5, true)]
        );
    }

    #[test]
    fn a_same_line_trailing_comment_extends_the_statement() {
        assert_eq!(
            lines("a(); /* x\n y */\nb();\n"),
            [(1, 2, false), (3, 3, false)]
        );
        assert_eq!(
            lines("a();\n/* x */ b();\n"),
            [(1, 1, false), (2, 2, false)]
        );
    }

    #[test]
    fn a_falls_through_comment_belongs_to_the_case_above() {
        let cases = |comment: &str| {
            let text = format!(
                "switch (x) {{\n  case 1:\n    a();\n  // {comment}\n  case 2:\n    b();\n}}\n"
            );
            scanned("a.ts", &text, |_, scan| {
                let switch = &scan.lists[1];
                (switch.stmts[0].end_line, switch.stmts[1].start_line)
            })
        };

        assert_eq!(cases("falls through"), (4, 5));
        assert_eq!(cases("FallThrough, on purpose"), (4, 5));
        assert_eq!(cases("note"), (3, 4));
        assert_eq!(cases("\u{17f}alls through"), (3, 4));
        assert_eq!(cases("fallthrough \u{212a}"), (4, 5));
    }

    #[test]
    fn compact_gap_decisions() {
        let lone = |statement: &str| {
            format!("function f() {{\n  foo();\n{statement}\n  bar();\n  baz();\n}}\n")
        };
        let walled = |statement: &str| {
            format!(
                "function f() {{\n  one();\n  two();\n{statement}\n  three();\n  four();\n  five();\n}}\n"
            )
        };

        let wide = "  if (a)\n    return someVeryLongFunctionName(argumentNumberOne, argumentNumberTwo, argumentNumberThree);";
        type Found = Vec<(usize, usize, Rule)>;

        let cases: Vec<(&str, String, Found)> = vec![
            ("a two-line if", lone("  if (a)\n    count = 1;"), vec![]),
            ("its one-line twin", lone("  if (a) count = 1;"), vec![]),
            (
                "an if with a two-line else",
                lone("  if (a)\n    count = 1;\n  else\n    count = 2;"),
                vec![],
            ),
            (
                "an if with else already one line per arm",
                lone("  if (a) count = 1;\n  else count = 2;"),
                vec![],
            ),
            (
                "a two-line for",
                lone("  for (; a;)\n    count = 1;"),
                vec![],
            ),
            (
                "a two-line while",
                lone("  while (a)\n    count = 1;"),
                vec![],
            ),
            (
                "a two-line for of",
                lone("  for (const x of xs)\n    count += x;"),
                vec![],
            ),
            (
                "a two-line guard, like its one-line twin",
                lone("  if (a)\n    return;"),
                vec![(5, 3, Rule::AfterGuard)],
            ),
            (
                "a comment between header and body",
                lone("  if (a)\n    // why\n    count = 1;"),
                vec![(3, 3, Rule::BlockSpacing), (6, 3, Rule::AfterMultiline)],
            ),
            (
                "a body too wide to join",
                lone(wide),
                vec![(3, 3, Rule::BlockSpacing), (5, 3, Rule::AfterMultiline)],
            ),
            (
                "a two-line if in a wall",
                walled("  if (a)\n    count = 1;"),
                vec![(2, 3, Rule::Wall)],
            ),
            (
                "a one-line if in a wall",
                walled("  if (a) count = 1;"),
                vec![(2, 3, Rule::Wall)],
            ),
            (
                "a call over several lines in a wall",
                walled("  run(\n    a,\n  );"),
                vec![(7, 3, Rule::AfterMultiline)],
            ),
        ];

        for (name, source, expected) in cases {
            let checked = process_file("a.ts", &source, Mode::Check, false, FALLBACK_LAYOUT, None);
            let found: Found = checked
                .findings
                .iter()
                .map(|finding| (finding.line, finding.col, finding.rule))
                .collect();

            assert_eq!(found, expected, "{name}");

            if expected.is_empty() {
                let fixed = process_file("a.ts", &source, Mode::Fix, false, FALLBACK_LAYOUT, None);
                assert!(fixed.fixed.is_none(), "{name}");
            }
        }
    }
}
