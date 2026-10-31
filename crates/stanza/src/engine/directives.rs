use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::text::{is_js_whitespace, js_trim};

use super::doc::{comment_index, line_at};
use super::model::{Comment, CommentKind, Doc, ListKind, StatementList};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Region {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Directive {
    Ignore,
    Off,
    On,
}

static ID_CONTINUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{ID_Continue}").expect("the pattern compiles"));

fn directive(comment: &Comment) -> Option<Directive> {
    let value = match comment.kind {
        CommentKind::Block => comment
            .value
            .trim_start_matches(|char| char == '*' || is_js_whitespace(char)),
        CommentKind::Line => comment.value,
    };

    let rest = js_trim(value).strip_prefix("stanza-")?;
    let (directive, after) = [
        ("ignore", Directive::Ignore),
        ("off", Directive::Off),
        ("on", Directive::On),
    ]
    .into_iter()
    .find_map(|(word, directive)| rest.strip_prefix(word).map(|after| (directive, after)))?;

    (!after.starts_with('-') && !ID_CONTINUE.is_match(after)).then_some(directive)
}

fn whitespace(text: &str) -> bool {
    text.chars().all(is_js_whitespace)
}

fn directly_above(doc: &Doc, comment: &Comment, top: usize) -> bool {
    let Some(between) = doc.text.get(comment.end..top) else {
        return false;
    };

    let line_start = doc.line_starts[line_at(doc, comment.start) - 1];
    between.matches('\n').count() == 1
        && whitespace(between)
        && whitespace(&doc.text[line_start..comment.start])
}

pub fn ignored(doc: &Doc, start: usize) -> bool {
    let mut top = start;
    for comment in doc.comments[..comment_index(doc, start)].iter().rev() {
        if !directly_above(doc, comment, top) {
            return false;
        }

        if directive(comment) == Some(Directive::Ignore) {
            return true;
        }

        top = comment.start;
    }

    false
}

fn whole(doc: &Doc) -> Region {
    Region {
        start: 0,
        end: doc.text.len(),
    }
}

fn enclosing(doc: &Doc, containers: &[Region], comment: &Comment) -> Region {
    let mut block = whole(doc);
    for container in containers {
        if container.start < comment.start
            && comment.end <= container.end
            && (container.start > block.start
                || (container.start == block.start && container.end < block.end))
        {
            block = *container;
        }
    }

    block
}

fn nests(sorted: &[Region]) -> bool {
    let mut open: Vec<Region> = Vec::new();
    for container in sorted {
        while open.last().is_some_and(|last| last.end <= container.start) {
            open.pop();
        }

        if open.last().is_some_and(|last| container.end > last.end) {
            return false;
        }

        open.push(*container);
    }

    true
}

fn enclosing_all(doc: &Doc, containers: &[Region], marks: &[Comment]) -> Vec<Region> {
    let mut sorted = containers.to_vec();
    sorted.sort_by(|left, right| left.start.cmp(&right.start).then(right.end.cmp(&left.end)));

    if !nests(&sorted) {
        return marks
            .iter()
            .map(|comment| enclosing(doc, containers, comment))
            .collect();
    }

    let mut open: Vec<Region> = Vec::new();
    let mut next = 0;
    marks
        .iter()
        .map(|comment| {
            while next < sorted.len() && sorted[next].start < comment.start {
                while open
                    .last()
                    .is_some_and(|last| last.end <= sorted[next].start)
                {
                    open.pop();
                }

                open.push(sorted[next]);
                next += 1;
            }

            while open.last().is_some_and(|last| last.end < comment.end) {
                open.pop();
            }

            open.last().copied().unwrap_or_else(|| whole(doc))
        })
        .collect()
}

pub fn marks<'a>(doc: &Doc<'a>) -> Vec<Comment<'a>> {
    doc.comments
        .iter()
        .filter(|comment| matches!(directive(comment), Some(Directive::Off | Directive::On)))
        .copied()
        .collect()
}

struct Open {
    start: usize,
    depth: usize,
}

pub fn regions(doc: &Doc, marks: &[Comment], containers: &[Region]) -> Vec<Region> {
    if marks.is_empty() {
        return Vec::new();
    }

    let ranges = enclosing_all(doc, containers, marks);
    let mut open: HashMap<Region, Open> = HashMap::new();
    let mut result = Vec::new();
    for (comment, range) in marks.iter().zip(ranges) {
        if directive(comment) == Some(Directive::Off) {
            open.entry(range)
                .or_insert(Open {
                    start: comment.start,
                    depth: 0,
                })
                .depth += 1;

            continue;
        }

        let Some(current) = open.get_mut(&range) else {
            continue;
        };

        current.depth -= 1;
        if current.depth > 0 {
            continue;
        }

        result.push(Region {
            start: current.start,
            end: comment.start,
        });

        open.remove(&range);
    }

    result.extend(open.into_iter().map(|(range, region)| Region {
        start: region.start,
        end: range.end,
    }));

    result.sort_by_key(|region| region.start);

    let mut merged: Vec<Region> = Vec::new();
    for region in result {
        match merged.last_mut() {
            Some(previous) if region.start <= previous.end => {
                previous.end = previous.end.max(region.end);
            }
            _ => merged.push(region),
        }
    }

    merged
}

pub fn within(regions: &[Region], offset: usize) -> bool {
    let low = regions.partition_point(|region| region.start <= offset);
    low > 0 && offset < regions[low - 1].end
}

pub fn freeze<N>(
    doc: &Doc,
    marked: &[Comment],
    mut lists: Vec<StatementList<N>>,
    containers: &[Region],
) -> (Vec<StatementList<N>>, Vec<Region>) {
    if marked.is_empty() {
        return (lists, Vec::new());
    }

    let frozen = regions(doc, marked, containers);
    for stmt in lists.iter_mut().flat_map(|list| list.stmts.iter_mut()) {
        if within(&frozen, stmt.start) {
            stmt.frozen = true;
        }
    }

    lists.retain(|list| list.kind == ListKind::Module || !within(&frozen, list.start));
    (lists, frozen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::javascript::scan::scanned;

    fn comment(kind: CommentKind, value: &str) -> Comment<'_> {
        Comment {
            kind,
            value,
            start: 0,
            end: 0,
        }
    }

    #[test]
    fn directives_match_the_trimmed_value() {
        let line = |value| directive(&comment(CommentKind::Line, value));
        let block = |value| directive(&comment(CommentKind::Block, value));
        assert!(line("  stanza-off \u{feff}") == Some(Directive::Off));
        assert!(line("stanza-on: done") == Some(Directive::On));
        assert!(line("stanza-ignore next") == Some(Directive::Ignore));
        assert!(block("*\n   * stanza-ignore\n   ") == Some(Directive::Ignore));
        assert!(line("* stanza-off").is_none());
        assert!(line("\u{85}stanza-off").is_none());
        assert!(line("note stanza-off").is_none());
    }

    #[test]
    fn an_identifier_character_or_dash_breaks_the_match() {
        for value in [
            "stanza-offx",
            "stanza-off-now",
            "stanza-offé",
            "stanza-off\u{b7}",
            "stanza-on1",
        ] {
            assert!(
                directive(&comment(CommentKind::Line, value)).is_none(),
                "{value}"
            );
        }

        assert!(directive(&comment(CommentKind::Line, "stanza-off.")) == Some(Directive::Off));
    }

    fn frozen_text(path: &str, text: &str) -> Vec<String> {
        scanned(path, text, |doc, scan| {
            scan.frozen
                .iter()
                .map(|region| doc.text[region.start..region.end].to_owned())
                .collect()
        })
    }

    #[test]
    fn an_unterminated_off_runs_to_the_end_of_its_container() {
        let text = "function a() {\n  b();\n  // stanza-off\n  c();\n}\nd();\n";
        assert_eq!(frozen_text("a.ts", text), ["// stanza-off\n  c();\n}"]);
        assert_eq!(
            frozen_text("a.ts", "a();\n// stanza-off\nb();\n"),
            ["// stanza-off\nb();\n"]
        );
    }

    #[test]
    fn on_closes_only_an_off_in_the_same_container() {
        let text = "function a() {\n  if (x) {\n    // stanza-off\n    y();\n  }\n  // stanza-on\n  z();\n}\n";
        assert_eq!(frozen_text("a.ts", text), ["// stanza-off\n    y();\n  }"]);
    }

    #[test]
    fn nested_offs_need_as_many_ons() {
        let text = "// stanza-off\n// stanza-off\na();\n// stanza-on\nb();\n// stanza-on\nc();\n";
        assert_eq!(
            frozen_text("a.ts", text),
            ["// stanza-off\n// stanza-off\na();\n// stanza-on\nb();\n"]
        );
    }

    #[test]
    fn a_decorator_outside_its_parameter_is_no_container() {
        let text =
            "class A {\n  constructor(@d(() => {\n    // stanza-off\n  }) x: T) {}\n  m() {}\n}\n";
        assert_eq!(
            frozen_text("a.ts", text),
            ["// stanza-off\n  }) x: T) {}\n  m() {}\n}"]
        );

        let optional = text.replace("x: T", "x?: T");
        assert_eq!(frozen_text("a.ts", &optional), ["// stanza-off\n  }"]);
    }

    #[test]
    fn ignore_needs_an_unbroken_comment_stack_above() {
        let frozen = |text| {
            scanned("a.ts", text, |_, scan| {
                scan.lists[0]
                    .stmts
                    .iter()
                    .map(|stmt| stmt.frozen)
                    .collect::<Vec<_>>()
            })
        };

        assert_eq!(
            frozen("// stanza-ignore\n// why\na();\nb();\n"),
            [true, false]
        );
        assert_eq!(frozen("// stanza-ignore\n\na();\n"), [false]);
        assert_eq!(frozen("x; // stanza-ignore\na();\n"), [false, false]);
    }
}
