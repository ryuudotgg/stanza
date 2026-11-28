use std::collections::HashSet;
use std::marker::PhantomData;
use std::num::NonZeroUsize;

use crate::languages::{BraceRule, Language, Limit, Settings, WidthFrom, WidthSetting};
use crate::os::reason;
use crate::text::js_trim;

use super::directives::{Region, within};
use super::doc::{line_at, position};
use super::edits::{OffsetMap, back_through};
use super::gaps::{
    Decision, Gap, Outcome, Ruled, WALL, block_spacing, list_gaps, matches, outcome,
    short_body_shape, walls,
};
use super::hunks::touching;
use super::model::{Doc, Reads, StatementList, Stmt};
use super::rules::{Rule, Want};
use super::types::Braces;
use super::{Driver, Passes};

pub struct Request<'r> {
    pub path: &'r str,
    pub text: &'r str,
    pub display: &'r dyn Fn(&str) -> String,
}

pub struct Opening {
    pub start: usize,
    pub open: usize,
    pub close: usize,
    pub inner: Option<(usize, usize)>,
}

pub enum Verdict {
    Directive,
    Inapplicable(String),
    Applies { held_by: Option<String> },
}

pub struct Explained {
    pub found: bool,
    pub lines: Vec<String>,
}

struct Traced<'t, N> {
    original: &'t Doc<'t>,
    frozen: &'t [Region],
    openings: &'t dyn Fn(usize) -> Vec<Opening>,
    doc: &'t Doc<'t>,
    lists: &'t [StatementList<N>],
    verdict: &'t dyn Fn(usize) -> Verdict,
    maps: &'t [OffsetMap],
    unbraced: &'t [usize],
    spans: &'t [(usize, Span)],
    width: &'t WidthSetting,
    display: &'t dyn Fn(&str) -> String,
}

#[derive(Clone, Copy)]
struct Span {
    from: usize,
    code: usize,
    to: usize,
}

const LABEL: usize = 11;

fn field(label: &str, text: &str) -> String {
    let label = if label.is_empty() {
        String::new()
    } else {
        format!("{label}:")
    };

    format!("{label:<LABEL$}{text}")
}

fn plural(count: usize, noun: &str) -> String {
    let number = if count == 0 {
        "no".to_owned()
    } else {
        count.to_string()
    };

    format!("{number} {noun}{}", if count == 1 { "" } else { "s" })
}

fn line_list(lines: &[usize]) -> String {
    match lines {
        [only] => format!("line {only}"),
        [rest @ .., last] => {
            let rest: Vec<String> = rest.iter().map(usize::to_string).collect();
            format!("lines {} and {last}", rest.join(", "))
        }
        [] => unreachable!("a note lists at least one line"),
    }
}

impl<N> Traced<'_, N> {
    fn original_line(&self, offset: usize) -> usize {
        line_at(self.original, back_through(self.maps, offset))
    }

    fn at(&self, line: usize) -> usize {
        self.original_line(self.doc.line_starts[line - 1])
    }

    fn code_line(&self, stmt: &Stmt<N>) -> usize {
        line_at(self.doc, stmt.start)
    }

    fn span(&self, stmt: &Stmt<N>) -> Span {
        let start = back_through(self.maps, stmt.start);
        match self.spans.binary_search_by_key(&start, |&(start, _)| start) {
            Ok(index) => self.spans[index].1,
            Err(_) => Span {
                from: self.at(stmt.start_line),
                code: line_at(self.original, start),
                to: self.original_line(stmt.end),
            },
        }
    }

    fn brace_note(&self, first: &Stmt<N>, last: &Stmt<N>) -> Option<String> {
        let within = back_through(self.maps, first.start)..back_through(self.maps, last.end);
        let mut unbraced: Vec<usize> = self
            .unbraced
            .iter()
            .copied()
            .filter(|offset| within.contains(offset))
            .map(|offset| line_at(self.original, offset))
            .collect();

        unbraced.sort_unstable();
        unbraced.dedup();

        if unbraced.is_empty() {
            return None;
        }

        let note = format!(
            "decided after --fix removes the braces on {}",
            line_list(&unbraced)
        );

        Some(field("note", &note))
    }

    fn excerpt(&self, shown: &mut HashSet<usize>, first: usize, last: usize) -> Option<String> {
        if !shown.insert(first) {
            return None;
        }

        let label = if first == last {
            first.to_string()
        } else {
            format!("{first}-{last}")
        };

        let width = LABEL - 2;
        Some(format!(
            "  {label:<width$}{}",
            js_trim(self.original.lines[first - 1])
        ))
    }
}

fn wants(ruled: &Ruled) -> &'static str {
    match ruled.want() {
        Want::None => "no blank line",
        Want::AtLeastOne => "a blank line",
    }
}

fn because<N>(trace: &Traced<N>, list: &StatementList<N>, gap: &Gap<N>, ruled: &Ruled) -> String {
    let Gap { prev, next, .. } = gap;
    if let Some(join) = &ruled.join {
        return format!(
            "`{}` is bound on line {} and read by the `{}` below it",
            join.name,
            trace.at(trace.code_line(prev)),
            join.reader
        );
    }

    match ruled.rule {
        Rule::ShortBody => format!(
            "the block holds {} statements, each compact",
            list.stmts.len()
        ),
        Rule::GuardChain if prev.facts.joined.is_some() || next.facts.joined.is_some() => {
            match trace.width.layout.limit {
                Limit::Columns(_) => format!(
                    "both guards fit on one line at {} columns",
                    trace.width.columns_text
                ),
                Limit::Unlimited => {
                    "both guards fit on one line, the width is unlimited".to_owned()
                }
            }
        }
        Rule::GuardChain => "both statements are compact guards".to_owned(),
        Rule::AfterMultiline => format!(
            "the statement above spans lines {} to {}",
            trace.at(trace.code_line(prev)),
            trace.at(prev.end_line)
        ),
        Rule::SwitchClauses => "the clause above has a body".to_owned(),
        Rule::AfterGuard => {
            let jump = prev
                .facts
                .guard
                .and_then(|guard| guard.jump)
                .map_or("a jump".to_owned(), |jump| format!("`{jump}`"));

            format!(
                "the guard on line {} ends in {jump}, so the next statement starts a new step",
                trace.at(trace.code_line(prev))
            )
        }
        Rule::LetStep => format!(
            "the `let` on line {} joins the block below it, so it starts its own step",
            trace.at(trace.code_line(next))
        ),
        rule => unreachable!("{} is not a gap rule without a bound name", rule.id()),
    }
}

fn result<N>(trace: &Traced<N>, gap: &Gap<N>) -> String {
    match outcome(gap) {
        Outcome::Unruled => "--fix leaves the gap alone".to_owned(),
        Outcome::Detached => format!(
            "the comment above line {} is set apart by a blank line, so --fix leaves the gap alone",
            trace.at(trace.code_line(gap.next))
        ),
        Outcome::SharedLine => {
            "both statements share a line, so --fix leaves the gap alone".to_owned()
        }
        Outcome::Holds(Want::None) => "already has no blank line".to_owned(),
        Outcome::Holds(Want::AtLeastOne) => "already has a blank line".to_owned(),
        Outcome::Removes(blank) => format!("--fix removes {}", plural(blank, "blank line")),
        Outcome::Adds => "--fix adds a blank line".to_owned(),
    }
}

fn joined_line<N>(trace: &Traced<N>, stmt: &Stmt<N>) -> String {
    let joined = stmt
        .facts
        .joined
        .expect("only joined statements are described");
    let line = trace.at(trace.code_line(stmt));
    let fits = trace.width.layout.limit.holds(joined.columns);
    let columns = joined.columns;
    if joined.arms == 1 {
        let fit = if fits {
            "fits on one line"
        } else {
            "too long for one line"
        };

        return format!("line {line} joins to {columns} columns, {fit}");
    }

    let fit = if fits {
        "each fits on its own line"
    } else {
        "too long for its own line"
    };

    format!(
        "line {line} has {} arms, the widest joins to {columns} columns, {fit}",
        joined.arms
    )
}

fn width_label<N>(trace: &Traced<N>) -> String {
    match &trace.width.source {
        WidthFrom::Config(file) => (trace.display)(file),
        WidthFrom::Default(formatter) => format!("{formatter} default"),
        WidthFrom::Fallback => "fallback".to_owned(),
    }
}

fn explain_gap<N: Reads>(
    trace: &Traced<N>,
    shown: &mut HashSet<usize>,
    list: &StatementList<N>,
    gap: &Gap<N>,
) -> Vec<String> {
    let Gap {
        prev,
        next,
        decision,
        ..
    } = gap;

    let next_line = trace.at(trace.code_line(next));
    let mut lines = vec![format!(
        "gap above line {next_line}, {}",
        plural(gap.blank, "blank line")
    )];

    lines.extend(trace.excerpt(
        shown,
        trace.at(trace.code_line(prev)),
        trace.at(prev.end_line),
    ));
    lines.extend(trace.excerpt(shown, next_line, next_line));

    let (first, last) = if short_body_shape(list) {
        (&list.stmts[0], &list.stmts[list.stmts.len() - 1])
    } else {
        (*prev, *next)
    };

    lines.extend(trace.brace_note(first, last));

    let joined: Vec<&Stmt<N>> = [*prev, *next]
        .into_iter()
        .filter(|stmt| stmt.facts.joined.is_some())
        .collect();

    if !joined.is_empty() {
        let columns = match trace.width.layout.limit {
            Limit::Columns(_) => format!("{} columns", trace.width.columns_text),
            Limit::Unlimited => trace.width.columns_text.clone(),
        };

        let width = format!("{columns}, from {}", width_label(trace));

        lines.push(field("width", &width));
        lines.extend(
            joined
                .iter()
                .map(|stmt| field("", &joined_line(trace, stmt))),
        );
    }

    match decision {
        Decision::Frozen => lines.push(field(
            "rule",
            "none, a stanza directive covers one of the two statements",
        )),
        Decision::Keep => lines.push(field(
            "rule",
            "none applies, the blank lines stay as written",
        )),
        Decision::Ruled(ruled) => {
            lines.push(field(
                "rule",
                &format!("{}, wants {}", ruled.rule.id(), wants(ruled)),
            ));

            lines.push(field("because", &because(trace, list, gap, ruled)));

            let outranked: Vec<Ruled> = matches(trace.doc, list, prev, next)
                .filter(|other| other.rule != ruled.rule)
                .collect();

            if outranked.is_empty() {
                lines.push(field("outranked", "none"));
            }

            for (index, other) in outranked.iter().enumerate() {
                let label = if index == 0 { "outranked" } else { "" };
                let text = format!(
                    "{}, wants {}: {}",
                    other.rule.id(),
                    wants(other),
                    because(trace, list, gap, other)
                );

                lines.push(field(label, &text));
            }
        }
    }

    lines.push(field("result", &result(trace, gap)));

    if let Some(reported) = block_spacing(trace.doc, gap) {
        lines.push(field(
            "reported",
            &format!("block-spacing, {}", reported.message),
        ));
    }

    lines
}

fn explain_wall<N>(trace: &Traced<N>, shown: &mut HashSet<usize>, run: &[Stmt<N>]) -> Vec<String> {
    let head = trace.span(&run[0]);
    let tail = trace.span(&run[run.len() - 1]);
    let mut lines = Vec::new();
    if head.code == tail.to {
        lines.push(format!("wall on line {}", head.code));
        lines.extend(trace.excerpt(shown, head.code, head.code));
    } else {
        lines.push(format!("wall, lines {} to {}", head.code, tail.to));
        lines.extend(trace.excerpt(shown, head.code, head.to));
        lines.extend(trace.excerpt(shown, tail.code, tail.to));
    }

    lines.extend(trace.brace_note(&run[0], &run[run.len() - 1]));
    lines.extend([
        field(
            "rule",
            &format!("{}, wants a blank line where one step ends", Rule::Wall.id()),
        ),
        field(
            "because",
            &format!(
                "{} single-line statements run with no blank line --fix keeps, and {WALL} or more make a wall",
                run.len()
            ),
        ),
        field("result", "--fix leaves the split to you"),
    ]);

    lines
}

fn config_holds<L: Language>(request: &Request, braces: &BraceRule) -> Vec<String> {
    match braces {
        BraceRule::Flag(Braces::Off) => return vec!["--no-braces turns the rule off".to_owned()],
        BraceRule::Flag(Braces::On) => return Vec::new(),
        BraceRule::Config(setting) if !setting.enforced => return Vec::new(),
        BraceRule::Config(_) => {}
    }

    match L::brace_reasons(request.path) {
        Ok(holds) => holds
            .into_iter()
            .map(|hold| {
                let files: Vec<String> = hold
                    .files
                    .iter()
                    .map(|file| (request.display)(file))
                    .collect();

                let files = files.join(", ");
                if hold.enforced {
                    format!("{files} enforces braces")
                } else {
                    format!("stanza cannot tell whether {files} enforces braces, so it keeps them")
                }
            })
            .collect(),
        Err(error) => vec![format!(
            "stanza cannot read the lint config: {}, so it keeps them",
            reason(&error)
        )],
    }
}

fn explain_block<N>(
    trace: &Traced<N>,
    shown: &mut HashSet<usize>,
    block: &Opening,
    config: &[String],
) -> Vec<String> {
    let Opening {
        open, close, inner, ..
    } = *block;

    let mut lines = vec![if open == close {
        format!("braced body on line {open}")
    } else {
        format!("braced body, lines {open} to {close}")
    }];

    if let Some((first, last)) = inner {
        lines.extend(trace.excerpt(shown, first, last));
    }

    let rule = field(
        "rule",
        &format!(
            "braces, {}",
            Rule::Braces
                .entry()
                .expect("braces is a catalog rule")
                .summary
        ),
    );

    let mut offset = block.start;
    for (pass, map) in trace.maps.iter().enumerate() {
        if map.removes(offset) {
            let result = if pass == 0 {
                "--fix removes the braces"
            } else {
                "--fix removes the braces once the braces inside them are gone"
            };

            lines.extend([rule, field("result", result)]);
            return lines;
        }

        offset = map.forward(offset);
    }

    let reason = match (trace.verdict)(offset) {
        Verdict::Directive => {
            lines.push(field(
                "rule",
                "braces, but a stanza directive covers this body",
            ));

            return lines;
        }
        Verdict::Inapplicable(reason) => {
            lines.push(field("rule", &format!("braces does not apply, {reason}")));
            return lines;
        }
        Verdict::Applies { held_by } => held_by,
    };

    let reasons: Vec<&str> = config
        .iter()
        .map(String::as_str)
        .chain(reason.as_deref())
        .collect();

    lines.push(rule);
    for (index, reason) in reasons.iter().enumerate() {
        lines.push(field(if index == 0 { "kept by" } else { "" }, reason));
    }

    let result = if reasons.is_empty() {
        "--fix removes the braces"
    } else {
        "the braces stay"
    };

    lines.push(field("result", result));
    lines
}

#[derive(Debug)]
pub struct PastEnd {
    pub path: String,
    pub lines: usize,
}

impl PastEnd {
    pub fn message(&self, asked: impl std::fmt::Display) -> String {
        let noun = if self.lines == 1 { "line" } else { "lines" };
        format!("{} has {} {noun}, not {asked}", self.path, self.lines)
    }
}

pub type Line = Result<Explained, PastEnd>;

struct ByLine<T> {
    found: Vec<T>,
    by_line: Vec<(usize, usize)>,
}

impl<T> ByLine<T> {
    fn new<L: IntoIterator<Item = usize>>(found: Vec<T>, lines: impl Fn(&T) -> L) -> Self {
        let mut by_line: Vec<(usize, usize)> = found
            .iter()
            .enumerate()
            .flat_map(|(index, item)| lines(item).into_iter().map(move |line| (line, index)))
            .collect();

        by_line.sort_by_key(|&(line, _)| line);
        ByLine { found, by_line }
    }

    fn on(&self, line: usize) -> impl Iterator<Item = &T> {
        let from = self.by_line.partition_point(|&(at, _)| at < line);
        self.by_line[from..]
            .iter()
            .take_while(move |&&(at, _)| at == line)
            .map(|&(_, index)| &self.found[index])
    }
}

struct Index<'g, N> {
    gaps: ByLine<(&'g StatementList<N>, Gap<'g, N>)>,
    walls: ByLine<&'g [Stmt<N>]>,
}

fn index<'g, N: Reads>(trace: &Traced<'g, N>) -> Index<'g, N> {
    let mut gaps = Vec::new();
    let mut found_walls = Vec::new();
    for list in trace.lists {
        let found = list_gaps(trace.doc, list);
        found_walls.extend(walls(trace.doc, list, &found, &touching(None)));
        gaps.extend(found.into_iter().map(|gap| (list, gap)));
    }

    found_walls.sort_by_key(|run| back_through(trace.maps, run[0].start));

    Index {
        gaps: ByLine::new(gaps, |(_, gap)| {
            let code = trace.at(trace.code_line(gap.next));
            let start = trace.at(gap.next.start_line);
            [code].into_iter().chain((start != code).then_some(start))
        }),
        walls: ByLine::new(found_walls, |run| {
            trace.span(&run[0]).from..=trace.span(&run[run.len() - 1]).to
        }),
    }
}

fn describe<N: Reads>(
    trace: &Traced<N>,
    index: &Index<N>,
    request: &Request,
    asked: NonZeroUsize,
    config: &[String],
) -> Line {
    let original = trace.original;
    let lines = original.lines.len();
    let line_count = if original.lines.last() == Some(&"") {
        lines - 1
    } else {
        lines
    };

    let path = (request.display)(request.path);
    let line = asked.get();
    if line > line_count {
        return Err(PastEnd {
            path,
            lines: line_count,
        });
    }

    let mut shown = HashSet::new();
    let mut sections: Vec<Vec<String>> = Vec::new();
    for (list, gap) in index.gaps.on(line) {
        sections.push(explain_gap(trace, &mut shown, list, gap));
    }

    for block in (trace.openings)(line) {
        sections.push(explain_block(trace, &mut shown, &block, config));
    }

    for run in index.walls.on(line) {
        sections.push(explain_wall(trace, &mut shown, run));
    }

    if sections.is_empty() && within(trace.frozen, original.line_starts[line - 1]) {
        sections.push(vec![format!(
            "line {line} is inside a stanza-off region, so stanza leaves it alone"
        )]);
    }

    let header = format!("{path}:{line}");
    if sections.is_empty() {
        return Ok(Explained {
            found: false,
            lines: vec![format!(
                "{header}: no gap ends and no braced body starts on this line"
            )],
        });
    }

    let mut lines = vec![header];
    if !trace.width.unread.is_empty() {
        let files: Vec<String> = trace
            .width
            .unread
            .iter()
            .map(|file| (request.display)(file))
            .collect();

        lines.push(String::new());
        lines.push(field(
            "note",
            &format!("cannot read the line width from {}", files.join(", ")),
        ));
    }

    for section in sections {
        lines.push(String::new());
        lines.extend(section);
    }

    Ok(Explained { found: true, lines })
}

pub(crate) fn explain<L: Language>(
    request: &Request,
    settings: &Settings,
    lines: &[NonZeroUsize],
) -> Result<Vec<Line>, String> {
    let Request { path, text, .. } = *request;
    let layout = settings.layout();
    let explained = L::examine_source(path, text, layout, |original, first| {
        let config = config_holds::<L>(request, &settings.braces);
        let keep_braces = settings.braces.keeps();
        let mut spans: Vec<(usize, Span)> = first
            .lists
            .iter()
            .flat_map(|list| &list.stmts)
            .map(|stmt| {
                let span = Span {
                    from: stmt.start_line,
                    code: line_at(original, stmt.start),
                    to: stmt.end_line,
                };

                (stmt.start, span)
            })
            .collect();

        spans.sort_unstable_by_key(|&(start, _)| start);
        let mut driver = Driver::<L> {
            path,
            layout,
            keep_braces,
            lines: None,
            passes: Some(Passes::default()),
            language: PhantomData,
        };

        let openings = |line: usize| L::openings(original, &first.bodies, line);
        driver.settle(original, first, |doc, scanned, _, driver| {
            let passes = driver.passes.as_ref().expect("explain traces every pass");
            let verdict = |offset: usize| {
                let at = |moved: usize| line_at(original, back_through(&passes.maps, moved));
                L::verdict(doc, &scanned.eligible, offset, &at)
            };

            let traced = Traced {
                original,
                frozen: &first.frozen,
                openings: &openings,
                doc,
                lists: &scanned.lists,
                verdict: &verdict,
                maps: &passes.maps,
                unbraced: &passes.unbraced,
                spans: &spans,
                width: &settings.width,
                display: request.display,
            };

            let index = index(&traced);
            lines
                .iter()
                .map(|&line| describe(&traced, &index, request, line, &config))
                .collect()
        })
    });

    explained.map_err(|rejected| {
        format!(
            "{}:{} does not parse: {}",
            (request.display)(path),
            position(text, rejected.start).0,
            rejected.message
        )
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::super::tests::compact_nesting;
    use super::Line;
    use crate::engine::doc::line_at;
    use crate::engine::gaps::{Spacing, spacing};
    use crate::engine::hunks::touching;
    use crate::engine::rules::Rule;
    use crate::engine::types::Braces;
    use crate::languages::javascript::scan::scanned;
    use crate::languages::source_entry;
    use crate::testing::timing::{LINEAR_GROWTH_CAP, assert_bounded, cpu_ms, on_deep_stack, ratio};

    struct Request<'r> {
        path: &'r str,
        text: &'r str,
        braces: Option<Braces>,
        display: &'r dyn Fn(&str) -> String,
    }

    fn explain(request: &Request, lines: &[NonZeroUsize]) -> Result<Vec<Line>, String> {
        let entry = source_entry(request.path);
        let settings = entry.settings(request.path, request.braces);
        let request = super::Request {
            path: request.path,
            text: request.text,
            display: request.display,
        };

        entry.explain(&request, &settings, lines)
    }

    fn every_gap(count: usize) -> (String, Vec<NonZeroUsize>) {
        let text = format!("function f() {{\n{}}}\n", "  a();\n".repeat(count));
        let lines = (3..=count + 1).filter_map(NonZeroUsize::new).collect();
        (text, lines)
    }

    fn explain_every_gap((text, lines): &(String, Vec<NonZeroUsize>)) -> Vec<bool> {
        let display = |path: &str| path.to_owned();
        let request = Request {
            path: "large.ts",
            text,
            braces: Some(Braces::On),
            display: &display,
        };

        let explained = explain(&request, lines).expect("the text parses");
        explained
            .iter()
            .map(|line| line.as_ref().is_ok_and(|line| line.found))
            .collect()
    }

    #[test]
    fn a_gap_explains_once_from_its_comment_line_and_its_code_line() {
        let text = "function f() {\n  a();\n  // why\n  b();\n  c();\n}\n";
        let display = |path: &str| path.to_owned();
        let request = Request {
            path: "comment.ts",
            text,
            braces: Some(Braces::On),
            display: &display,
        };

        let lines: Vec<NonZeroUsize> = [3, 4, 5]
            .into_iter()
            .filter_map(NonZeroUsize::new)
            .collect();
        let explained = explain(&request, &lines).expect("the text parses");
        let gaps: Vec<Vec<&str>> = explained
            .iter()
            .map(|line| {
                let line = line.as_ref().expect("each line is in the file");
                line.lines
                    .iter()
                    .map(String::as_str)
                    .filter(|line| line.starts_with("gap above"))
                    .collect()
            })
            .collect();

        assert_eq!(
            gaps,
            [
                vec!["gap above line 4, no blank lines"],
                vec!["gap above line 4, no blank lines"],
                vec!["gap above line 5, no blank lines"],
            ]
        );
        assert_eq!(
            explained[0].as_ref().unwrap().lines[1..],
            explained[1].as_ref().unwrap().lines[1..]
        );
    }

    #[test]
    fn the_brace_note_names_each_removed_block_once() {
        let text = "if (a) {\n  three();\n\n} else {\n  four();\n  five();\n}\nnext();\nif (b) {\n  c();\n}\nlast();\n";
        let display = |path: &str| path.to_owned();
        let request = Request {
            path: "blanks.ts",
            text,
            braces: Some(Braces::On),
            display: &display,
        };

        let lines: Vec<NonZeroUsize> = [9].into_iter().filter_map(NonZeroUsize::new).collect();
        let explained = explain(&request, &lines).expect("the text parses");
        let notes: Vec<&str> = explained[0]
            .as_ref()
            .expect("the line is in the file")
            .lines
            .iter()
            .map(String::as_str)
            .filter(|line| line.starts_with("note"))
            .collect();

        assert_eq!(
            notes,
            ["note:      decided after --fix removes the braces on line 9"]
        );
    }

    fn gap_notes_and_rule(text: &str, line: usize) -> Vec<String> {
        let display = |path: &str| path.to_owned();
        let request = Request {
            path: "note.ts",
            text,
            braces: Some(Braces::On),
            display: &display,
        };

        let lines: Vec<NonZeroUsize> = NonZeroUsize::new(line).into_iter().collect();
        let explained = explain(&request, &lines).expect("the text parses");
        explained[0]
            .as_ref()
            .expect("the line is in the file")
            .lines
            .iter()
            .filter(|line| line.starts_with("note:") || line.starts_with("rule:"))
            .cloned()
            .collect()
    }

    #[test]
    fn gap_notes_name_relevant_braces() {
        let module = "function unrelated() {\n  if (x) { f(); }\n}\nconst LIMIT = 3;\n\nexport function over(n) { return n > LIMIT; }\n";
        let short = "function f() {\n  if (x) {\n    run();\n  }\n\n  done();\n}\n";
        let long = "function f() {\n  if (a) { b(); }\n  one();\n  const x = load();\n\n  if (x) use(x);\n  done();\n}\n";
        let sibling = "function f() {\n  if (a) { b(); }\n  if (c) { d(); }\n\n  e();\n}\n";
        let shared = "function unrelated() { if (x) { f(); } } const LIMIT = 3;\n\nexport function over(n) { return n > LIMIT; }\n";
        let clause = "switch (value) {\n  case (() => {\n    if (x) { f(); }\n    return 1;\n  })():\n    a();\n\n    b();\n}\n";

        assert_eq!(
            gap_notes_and_rule(module, 6),
            ["rule:      use-join, wants no blank line"]
        );
        assert_eq!(
            gap_notes_and_rule(short, 6),
            [
                "note:      decided after --fix removes the braces on line 2",
                "rule:      short-body, wants no blank line",
            ]
        );
        assert_eq!(
            gap_notes_and_rule(long, 6),
            ["rule:      guard-join, wants no blank line"]
        );
        assert_eq!(
            gap_notes_and_rule(sibling, 5),
            [
                "note:      decided after --fix removes the braces on lines 2 and 3",
                "rule:      short-body, wants no blank line",
            ]
        );
        assert_eq!(
            gap_notes_and_rule(shared, 3),
            ["rule:      use-join, wants no blank line"]
        );
        assert_eq!(
            gap_notes_and_rule(clause, 8),
            ["rule:      short-body, wants no blank line"]
        );
    }

    fn wall_sections(text: &str, line: usize) -> Vec<String> {
        let display = |path: &str| path.to_owned();
        let request = Request {
            path: "wall.ts",
            text,
            braces: Some(Braces::On),
            display: &display,
        };

        let lines: Vec<NonZeroUsize> = NonZeroUsize::new(line).into_iter().collect();
        let explained = explain(&request, &lines).expect("the text parses");
        let explained = explained[0].as_ref().expect("the line is in the file");
        explained
            .lines
            .join("\n")
            .split("\n\n")
            .skip(1)
            .filter(|section| section.starts_with("wall"))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_wall_on_one_line_shows_that_line_once() {
        let text = "function f() {\n  a(); b(); c(); d(); e(); g();\n}\n";

        assert_eq!(
            wall_sections(text, 2),
            [
                "wall on line 2\nrule:      wall, wants a blank line where one step ends\nbecause:   6 single-line statements run with no blank line --fix keeps, and 6 or more make a wall\nresult:    --fix leaves the split to you"
            ]
        );
        assert_eq!(
            answers(text, &[2])[0]
                .matches("\n  2        a(); b(); c(); d(); e(); g();\n")
                .count(),
            1
        );
    }

    #[test]
    fn a_wall_ends_at_the_closing_brace_fix_removes() {
        let text = "function f() {\n  a();\n  b();\n  c();\n  d();\n  e();\n  if (x) {\n    y();\n  }\n}\n";
        let section = "wall, lines 2 to 9\n  2        a();\n  7-9      if (x) {\nnote:      decided after --fix removes the braces on line 7\nrule:      wall, wants a blank line where one step ends\nbecause:   6 single-line statements run with no blank line --fix keeps, and 6 or more make a wall\nresult:    --fix leaves the split to you";

        assert_eq!(wall_sections(text, 9), [section]);
        assert_eq!(wall_sections(text, 10), Vec::<String>::new());
    }

    #[test]
    fn a_wall_runs_from_the_comment_above_its_first_statement() {
        let text = "function f() {\n  // lead\n  a();\n  b();\n  c();\n  d();\n  e();\n  g();\n}\n";
        let header = |line| {
            wall_sections(text, line)
                .iter()
                .map(|section| section.lines().next().unwrap_or_default().to_owned())
                .collect::<Vec<String>>()
        };

        assert_eq!(header(2), ["wall, lines 3 to 8"]);
        assert_eq!(header(1), Vec::<String>::new());
    }

    #[test]
    fn a_wall_reads_its_lines_from_the_file_as_written() {
        let nested = "if (x) {\n  f(() => { a(); b(); c(); d(); e(); g(); });\n}\n";
        let section =
            "wall on line 2\n  2        f(() => { a(); b(); c(); d(); e(); g(); });\nrule:";

        assert!(wall_sections(nested, 2)[0].starts_with(section));
        assert_eq!(wall_sections(nested, 1), Vec::<String>::new());

        let trailing = "function f() {\n  a();\n  b();\n  c();\n  d();\n  e();\n  g(); /* note\n  tail */\n}\n";
        let section = "wall, lines 2 to 8\n  2        a();\n  7-8      g(); /* note\nrule:";

        assert!(wall_sections(trailing, 8)[0].starts_with(section));
    }

    #[test]
    fn a_line_inside_two_walls_shows_both_in_findings_order() {
        let text = "function f() {\n  a();\n  b();\n  c();\n  d();\n  e();\n  g(() => { h(); i(); j(); k(); l(); m(); });\n}\n";
        let headers: Vec<String> = wall_sections(text, 7)
            .iter()
            .map(|section| section.lines().next().unwrap_or_default().to_owned())
            .collect();

        assert_eq!(headers, ["wall, lines 2 to 7", "wall on line 7"]);
    }

    fn answers(text: &str, lines: &[usize]) -> Vec<String> {
        let display = |path: &str| path.to_owned();
        let request = Request {
            path: "once.ts",
            text,
            braces: Some(Braces::On),
            display: &display,
        };

        let lines: Vec<NonZeroUsize> = lines
            .iter()
            .filter_map(|&line| NonZeroUsize::new(line))
            .collect();
        explain(&request, &lines)
            .expect("the text parses")
            .into_iter()
            .map(|line| line.expect("each line is in the file").lines.join("\n"))
            .collect()
    }

    #[test]
    fn explain_excerpts_once() {
        let many = format!("{}f(){}", "if(a){".repeat(1_600), "}".repeat(1_600));
        let (many, explained) = on_deep_stack(move || {
            let explained = answers(&format!("{many}\n"), &[1]).remove(0);
            (many, explained)
        });

        assert_eq!(explained.matches(&many).count(), 1);
        assert_eq!(
            explained
                .lines()
                .filter(|line| line.starts_with("braced body"))
                .count(),
            1_600
        );
        assert!(explained.len() < 400_000);

        let two = "if (a) { f(\"雪\"); } if (b) { g(\"é\"); }\n";
        let explained = &answers(two, &[1])[0];

        assert_eq!(explained.matches(two.trim_end()).count(), 1);
        assert_eq!(
            explained
                .split("\n\n")
                .filter(|section| section.starts_with("braced body")
                    && section.ends_with("result:    --fix removes the braces"))
                .count(),
            2
        );

        let label = "if (a) {\n  f(\n    x);\n}\n";

        assert!(answers(label, &[1])[0].contains("\n  2-3      f(\n"));

        let shared = "if (a) {\n  if (b) { c(); }\n}\n";
        let row = "\n  2        if (b) { c(); }\n";
        let explained = answers(shared, &[1, 2, 2]);

        assert!(
            explained
                .iter()
                .all(|answer| answer.matches(row).count() == 1)
        );
    }

    #[derive(Debug, PartialEq)]
    struct Spaced {
        deleted: Vec<usize>,
        inserted: Vec<usize>,
        found: Vec<(Rule, usize)>,
        rule: String,
        result: String,
    }

    fn row(explained: &str, label: &str) -> String {
        explained
            .lines()
            .find(|row| row.starts_with(label))
            .unwrap_or_else(|| panic!("the gap has a {label} row"))
            .to_owned()
    }

    fn spaced_and_explained(text: &str, line: usize) -> Spaced {
        let (mut deleted, mut inserted, found) = scanned("gap.ts", text, |doc, scan| {
            let Spacing { edits, findings } =
                spacing(doc, &scan.lists, &scan.frozen, &touching(None));
            let found: Vec<(Rule, usize)> = findings
                .iter()
                .filter(|spot| spot.rule.fixable())
                .map(|spot| {
                    assert_eq!(spot.message, spot.rule.message(), "{text:?}");
                    (spot.rule, line_at(doc, spot.offset))
                })
                .collect();
            let deleted: Vec<usize> = edits.delete_lines.into_iter().collect();
            let inserted: Vec<usize> = edits.insert_after.into_iter().collect();
            (deleted, inserted, found)
        });

        deleted.sort_unstable();
        inserted.sort_unstable();

        let explained = answers(text, &[line]).remove(0);
        Spaced {
            deleted,
            inserted,
            found,
            rule: row(&explained, "rule:"),
            result: row(&explained, "result:"),
        }
    }

    fn expect(
        rule: &str,
        deleted: &[usize],
        inserted: &[usize],
        found: &[(Rule, usize)],
        result: &str,
    ) -> Spaced {
        Spaced {
            deleted: deleted.to_vec(),
            inserted: inserted.to_vec(),
            found: found.to_vec(),
            rule: format!("rule:      {rule}"),
            result: format!("result:    {result}"),
        }
    }

    #[test]
    fn shared_gap_outcomes_match_spacing() {
        let none = "if (a) throw e;\n";
        let one = "b();\n";
        let chain = "guard-chain, wants no blank line";
        let step = "after-guard, wants a blank line";
        let detached =
            "the comment above line 4 is set apart by a blank line, so --fix leaves the gap alone";
        let shared = "both statements share a line, so --fix leaves the gap alone";
        let cases = [
            (
                "a();\nb();\n".to_owned(),
                2,
                expect(
                    "none applies, the blank lines stay as written",
                    &[],
                    &[],
                    &[],
                    "--fix leaves the gap alone",
                ),
            ),
            (
                "// stanza-ignore\nif (a) throw e;\nif (b) throw e;\n".to_owned(),
                3,
                expect(
                    "none, a stanza directive covers one of the two statements",
                    &[],
                    &[],
                    &[],
                    "--fix leaves the gap alone",
                ),
            ),
            (
                format!("{none}// note\n\nif (b) throw e;\n"),
                4,
                expect(chain, &[], &[], &[], detached),
            ),
            (
                format!("{none}// note\n\n{one}"),
                4,
                expect(step, &[], &[], &[], detached),
            ),
            (
                "if (a) throw e; if (b) throw e;\n".to_owned(),
                1,
                expect(chain, &[], &[], &[], shared),
            ),
            (
                "if (a) throw e; b();\n".to_owned(),
                1,
                expect(step, &[], &[], &[], shared),
            ),
            (
                format!("{none}if (b) throw e;\n"),
                2,
                expect(chain, &[], &[], &[], "already has no blank line"),
            ),
            (
                format!("{none}\nif (b) throw e;\n"),
                3,
                expect(
                    chain,
                    &[2],
                    &[],
                    &[(Rule::GuardChain, 3)],
                    "--fix removes 1 blank line",
                ),
            ),
            (
                format!("{none}\n\nif (b) throw e;\n"),
                4,
                expect(
                    chain,
                    &[2, 3],
                    &[],
                    &[(Rule::GuardChain, 4)],
                    "--fix removes 2 blank lines",
                ),
            ),
            (
                format!("{none}{one}"),
                2,
                expect(
                    step,
                    &[],
                    &[1],
                    &[(Rule::AfterGuard, 2)],
                    "--fix adds a blank line",
                ),
            ),
            (
                format!("{none}\n{one}"),
                3,
                expect(step, &[], &[], &[], "already has a blank line"),
            ),
            (
                format!("{none}\n\n{one}"),
                4,
                expect(step, &[], &[], &[], "already has a blank line"),
            ),
        ];

        for (text, line, expected) in cases {
            assert_eq!(spaced_and_explained(&text, line), expected, "{text:?}");
        }
    }

    fn traced(text: &str, lines: &[usize]) -> Vec<String> {
        answers(text, lines)
            .into_iter()
            .map(|answer| {
                let rows: Vec<&str> = answer
                    .lines()
                    .filter(|row| !row.starts_with("width:"))
                    .collect();
                rows.join("\n")
            })
            .collect()
    }

    #[test]
    fn shared_brace_driver_keeps_explain_trace() {
        let long = "x".repeat(400);
        let join = "function f() {\n  if (a) {\n    b();\n  }\n  c();\n  d();\n  e();\n}\n";
        let wide = format!(
            "function f() {{\n  if (a) {{\n    {long}();\n  }}\n  c();\n  d();\n  e();\n}}\n"
        );
        let nested =
            "function f() {\n  if (a) {\n    if (b) {\n      c();\n    }\n  }\n  d();\n}\n";
        let later = "function f() {\n  if (a) {\n    if (b) {\n      c()\n  }}\n  d();\n}\n";
        let plain = "function f() {\n  if (a) b();\n  c();\n  d();\n  e();\n}\n";
        let frozen =
            "function f() {\n  // stanza-ignore\n  if (a) { b(); }\n  if (b) { c(); }\n}\n";
        let rule =
            "rule:      braces, a control flow body that is a single statement has no braces";

        assert_eq!(
            traced(join, &[2, 5]),
            [
                format!("once.ts:2\n\nbraced body, lines 2 to 4\n  3        b();\n{rule}\nresult:    --fix removes the braces"),
                "once.ts:5\n\ngap above line 5, no blank lines\n  2        if (a) {\n  5        c();\nnote:      decided after --fix removes the braces on line 2\nrule:      none applies, the blank lines stay as written\nresult:    --fix leaves the gap alone".to_owned(),
            ]
        );
        assert_eq!(
            traced(&wide, &[5]),
            [
                "once.ts:5\n\ngap above line 5, no blank lines\n  2-3      if (a) {\n  5        c();\nnote:      decided after --fix removes the braces on line 2\n           line 2 joins to 412 columns, too long for one line\nrule:      after-multiline, wants a blank line\nbecause:   the statement above spans lines 2 to 3\noutranked: none\nresult:    --fix adds a blank line"
            ]
        );
        assert_eq!(
            traced(nested, &[2, 3, 7]),
            [
                format!("once.ts:2\n\nbraced body, lines 2 to 6\n  3-5      if (b) {{\n{rule}\nresult:    --fix removes the braces"),
                format!("once.ts:3\n\nbraced body, lines 3 to 5\n  4        c();\n{rule}\nresult:    --fix removes the braces"),
                "once.ts:7\n\ngap above line 7, no blank lines\n  2        if (a) {\n  7        d();\nnote:      decided after --fix removes the braces on lines 2 and 3\nrule:      short-body, wants no blank line\nbecause:   the block holds 2 statements, each compact\noutranked: none\nresult:    already has no blank line".to_owned(),
            ]
        );
        assert_eq!(
            traced(later, &[3, 6]),
            [
                format!("once.ts:3\n\nbraced body, lines 3 to 5\n  4        c()\n{rule}\nresult:    --fix removes the braces once the braces inside them are gone"),
                "once.ts:6\n\ngap above line 6, no blank lines\n  2-3      if (a) {\n  6        d();\nnote:      decided after --fix removes the braces on lines 2 and 3\n           line 2 joins to 19 columns, fits on one line\nrule:      short-body, wants no blank line\nbecause:   the block holds 2 statements, each compact\noutranked: none\nresult:    already has no blank line".to_owned(),
            ]
        );
        assert_eq!(
            traced(plain, &[3]),
            [
                "once.ts:3\n\ngap above line 3, no blank lines\n  2        if (a) b();\n  3        c();\nrule:      none applies, the blank lines stay as written\nresult:    --fix leaves the gap alone"
            ]
        );
        assert_eq!(
            traced(frozen, &[3, 4]),
            [
                "once.ts:3\n\nbraced body on line 3\n  3        if (a) { b(); }\nrule:      braces, but a stanza directive covers this body".to_owned(),
                format!("once.ts:4\n\ngap above line 4, no blank lines\n  3        if (a) {{ b(); }}\n  4        if (b) {{ c(); }}\nnote:      decided after --fix removes the braces on line 4\nrule:      none, a stanza directive covers one of the two statements\nresult:    --fix leaves the gap alone\n\nbraced body on line 4\n{rule}\nresult:    --fix removes the braces"),
            ]
        );
    }

    #[test]
    fn explaining_bodies_closed_on_one_line_stays_linear_in_depth() {
        on_deep_stack(|| {
            assert_bounded(
                "compact nesting",
                &[400, 1_600],
                3,
                ratio(LINEAR_GROWTH_CAP),
                compact_nesting,
                |text| {
                    let display = |path: &str| path.to_owned();
                    let request = Request {
                        path: "large.ts",
                        text,
                        braces: Some(Braces::On),
                        display: &display,
                    };

                    let line = NonZeroUsize::MIN;
                    cpu_ms(|| {
                        explain(&request, &[line]).expect("the text parses");
                    })
                },
            );
        });
    }

    #[test]
    fn explaining_every_gap_of_a_file_stays_linear() {
        assert_bounded(
            "gaps",
            &[500, 2_000],
            3,
            ratio(LINEAR_GROWTH_CAP),
            every_gap,
            |gaps| {
                let mut found = Vec::new();
                let ms = cpu_ms(|| found = explain_every_gap(gaps));
                assert!(found.into_iter().all(|found| found));
                ms
            },
        );
    }
}
