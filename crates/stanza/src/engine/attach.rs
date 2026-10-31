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

fn fallthrough_end(doc: &Doc, item: &Item, next: Option<&Item>, end: usize) -> usize {
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

pub fn attach(doc: &Doc, items: &[Item], opener: usize, limit: usize) -> Vec<Stmt> {
    let mut previous_end = opener;
    items
        .iter()
        .enumerate()
        .map(|(item_index, item)| {
            let item_start_line = line_at(doc, item.start);
            let item_end_line = line_at(doc, item.end - 1);
            let lead_line = match doc.comments.get(comment_index(doc, previous_end)) {
                Some(leading) if leading.end <= item.start => line_at(doc, leading.start),
                _ => item_start_line,
            };

            let detached = lead_line != item_start_line
                && blank_lines(doc, lead_line - 1, item_start_line)
                    .next()
                    .is_some();

            let mut end = item.end;
            for comment in &doc.comments[comment_index(doc, item.end)..] {
                if comment.start >= limit || line_at(doc, comment.start) != item_end_line {
                    break;
                }

                end = end.max(comment.end);
            }

            if item.facts.kind == Kind::Case {
                end = fallthrough_end(doc, item, items.get(item_index + 1), end);
            }

            previous_end = end;
            Stmt {
                start: item.start,
                end: item.end,
                facts: item.facts,
                frozen: ignored(doc, item.start),
                start_line: if detached { item_start_line } else { lead_line },
                end_line: line_at(doc, end - 1),
                multiline: item_start_line != item_end_line,
                detached,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
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
}
