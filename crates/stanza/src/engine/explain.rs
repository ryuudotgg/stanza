use std::collections::HashSet;
use std::num::NonZeroUsize;

use oxc_allocator::Allocator;

use crate::files::reason;
use crate::languages::Layout;
use crate::languages::javascript::braces::{
    BracePass, Opening, Verdict, brace_edits, openings, verdict,
};
use crate::languages::javascript::config::brace_decisions;
use crate::languages::javascript::config::layers::{Setting, Target};
use crate::languages::javascript::config::width::{Width, WidthSource, formatter_width};
use crate::languages::javascript::facts::Node;
use crate::languages::javascript::lists::List;
use crate::languages::javascript::parse::parse_source;
use crate::languages::javascript::scan::{Scan, comments, examine, scan};
use crate::text::js_trim;

use super::directives::within;
use super::doc::{document, line_at, position};
use super::edits::{OffsetMap, apply_offsets, back_through};
use super::gaps::{
    Decision, Gap, Ruled, WALL, block_spacing, list_gaps, matches, short_body_shape, walls,
};
use super::hunks::touching;
use super::model::{Doc, Stmt};
use super::rules::{Rule, Want};
use super::types::Braces;

pub struct Request<'r> {
    pub path: &'r str,
    pub text: &'r str,
    pub braces: Option<Braces>,
    pub display: &'r dyn Fn(&str) -> String,
}

pub struct Explained {
    pub found: bool,
    pub lines: Vec<String>,
}

struct Traced<'t> {
    original: &'t Doc<'t>,
    first: &'t Scan<'t>,
    doc: &'t Doc<'t>,
    scanned: &'t Scan<'t>,
    maps: &'t [OffsetMap],
    unbraced: &'t [usize],
    spans: &'t [(usize, Span)],
    width: &'t Width,
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

impl Traced<'_> {
    fn original_line(&self, offset: usize) -> usize {
        line_at(self.original, back_through(self.maps, offset))
    }

    fn at(&self, line: usize) -> usize {
        self.original_line(self.doc.line_starts[line - 1])
    }

    fn code_line(&self, stmt: &Stmt<Node>) -> usize {
        line_at(self.doc, stmt.start)
    }

    fn span(&self, stmt: &Stmt<Node>) -> Span {
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

    fn brace_note(&self, first: &Stmt<Node>, last: &Stmt<Node>) -> Option<String> {
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

fn because(trace: &Traced, list: &List, gap: &Gap<Node>, ruled: &Ruled) -> String {
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
        Rule::GuardChain if prev.facts.joined.is_some() || next.facts.joined.is_some() => format!(
            "both guards fit on one line at {} columns",
            trace.width.columns_text
        ),
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
            "the `{}` on line {} joins the block below it, so it starts its own step",
            next.facts
                .declaration
                .expect("a let step names a let like declaration")
                .keyword,
            trace.at(trace.code_line(next))
        ),
        rule => unreachable!("{} is not a gap rule without a bound name", rule.id()),
    }
}

fn gap_result(trace: &Traced, gap: &Gap<Node>) -> String {
    let Gap {
        prev,
        next,
        blank,
        decision,
    } = gap;

    let Decision::Ruled(ruled) = decision else {
        return "--fix leaves the gap alone".to_owned();
    };

    if next.detached {
        return format!(
            "the comment above line {} is set apart by a blank line, so --fix leaves the gap alone",
            trace.at(trace.code_line(next))
        );
    }

    if next.start_line <= prev.end_line {
        return "both statements share a line, so --fix leaves the gap alone".to_owned();
    }

    match (ruled.want(), *blank) {
        (Want::None, 0) => "already has no blank line".to_owned(),
        (Want::None, blank) => format!("--fix removes {}", plural(blank, "blank line")),
        (Want::AtLeastOne, 0) => "--fix adds a blank line".to_owned(),
        (Want::AtLeastOne, _) => "already has a blank line".to_owned(),
    }
}

fn joined_line(trace: &Traced, stmt: &Stmt<Node>) -> String {
    let joined = stmt
        .facts
        .joined
        .expect("only joined statements are described");
    let line = trace.at(trace.code_line(stmt));
    let fits = joined.columns <= trace.width.columns;
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

fn width_label(trace: &Traced) -> String {
    match &trace.width.source {
        WidthSource::Config(file) => (trace.display)(file),
        WidthSource::Default(formatter) => format!("{} default", formatter.name()),
        WidthSource::Fallback => "fallback".to_owned(),
    }
}

fn explain_gap(
    trace: &Traced,
    shown: &mut HashSet<usize>,
    list: &List,
    gap: &Gap<Node>,
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

    let joined: Vec<&Stmt<Node>> = [*prev, *next]
        .into_iter()
        .filter(|stmt| stmt.facts.joined.is_some())
        .collect();

    if !joined.is_empty() {
        let width = format!(
            "{} columns, from {}",
            trace.width.columns_text,
            width_label(trace)
        );

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

    lines.push(field("result", &gap_result(trace, gap)));

    if let Some(reported) = block_spacing(trace.doc, gap) {
        lines.push(field(
            "reported",
            &format!("block-spacing, {}", reported.message),
        ));
    }

    lines
}

fn explain_wall(trace: &Traced, shown: &mut HashSet<usize>, run: &[Stmt<Node>]) -> Vec<String> {
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

fn config_holds(request: &Request) -> Vec<String> {
    match request.braces {
        Some(Braces::Off) => return vec!["--no-braces turns the rule off".to_owned()],
        Some(Braces::On) => return Vec::new(),
        None => {}
    }

    match brace_decisions(&Target::file(request.path)) {
        Ok(decisions) => decisions
            .into_iter()
            .filter(|decision| decision.setting != Setting::Off)
            .map(|decision| {
                let files: Vec<String> = decision
                    .files
                    .iter()
                    .map(|file| (request.display)(file))
                    .collect();

                let files = files.join(", ");
                if decision.setting == Setting::On {
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

fn explain_block(
    trace: &Traced,
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

    let at = |moved: usize| trace.original_line(moved);
    let reason = match verdict(trace.doc, &trace.scanned.eligible, offset, &at) {
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

#[derive(Default)]
struct Passes {
    maps: Vec<OffsetMap>,
    unbraced: Vec<usize>,
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

enum Pass {
    Settled(Vec<Line>),
    Unbraced(BracePass),
}

type Done<'d> = dyn Fn(&Doc, &Scan, &Passes) -> Vec<Line> + 'd;

fn settle(doc: &Doc, scanned: &Scan, layout: Layout, passes: &Passes, done: &Done) -> Pass {
    let pass = brace_edits(doc, &scanned.eligible, layout);
    if pass.edits.is_empty() {
        Pass::Settled(done(doc, scanned, passes))
    } else {
        Pass::Unbraced(pass)
    }
}

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

struct Index<'g> {
    gaps: ByLine<(&'g List<'g>, Gap<'g, Node<'g>>)>,
    walls: ByLine<&'g [Stmt<Node<'g>>]>,
}

fn index<'g>(trace: &Traced<'g>) -> Index<'g> {
    let mut gaps = Vec::new();
    let mut found_walls = Vec::new();
    for list in &trace.scanned.lists {
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

fn describe(
    trace: &Traced,
    index: &Index,
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

    for block in openings(original, &trace.first.bodies, line) {
        sections.push(explain_block(trace, &mut shown, &block, config));
    }

    for run in index.walls.on(line) {
        sections.push(explain_wall(trace, &mut shown, run));
    }

    if sections.is_empty() && within(&trace.first.frozen, original.line_starts[line - 1]) {
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

pub fn explain(request: &Request, lines: &[NonZeroUsize]) -> Result<Vec<Line>, String> {
    let Request { path, text, .. } = *request;
    let allocator = Allocator::default();
    let parsed = parse_source(&allocator, path, text).map_err(|rejected| {
        format!(
            "{}:{} does not parse: {}",
            (request.display)(path),
            position(text, rejected.start).0,
            rejected.message
        )
    })?;
    let original = document(text, comments(text, &parsed.program));

    let config = config_holds(request);
    let width = formatter_width(path);
    let layout = width.layout();
    let first = scan(&original, &parsed.program, layout);
    let mut spans: Vec<(usize, Span)> = first
        .lists
        .iter()
        .flat_map(|list| &list.stmts)
        .map(|stmt| {
            let span = Span {
                from: stmt.start_line,
                code: line_at(&original, stmt.start),
                to: stmt.end_line,
            };

            (stmt.start, span)
        })
        .collect();

    spans.sort_unstable_by_key(|&(start, _)| start);
    let done = |doc: &Doc, scanned: &Scan, passes: &Passes| {
        let traced = Traced {
            original: &original,
            first: &first,
            doc,
            scanned,
            maps: &passes.maps,
            unbraced: &passes.unbraced,
            spans: &spans,
            width: &width,
            display: request.display,
        };

        let index = index(&traced);
        lines
            .iter()
            .map(|&line| describe(&traced, &index, request, line, &config))
            .collect()
    };

    let mut passes = Passes::default();
    if !config.is_empty() {
        return Ok(done(&original, &first, &passes));
    }

    let mut current = original.text.to_owned();
    let mut pass = settle(&original, &first, layout, &passes, &done);
    loop {
        let BracePass { edits, starts } = match pass {
            Pass::Settled(explained) => return Ok(explained),
            Pass::Unbraced(pass) => pass,
        };

        for start in starts {
            passes.unbraced.push(back_through(&passes.maps, start));
        }

        passes.maps.push(OffsetMap::new(&edits));
        current = apply_offsets(&current, &edits);
        pass = examine(path, &current, layout, |doc, scanned, _| {
            settle(doc, scanned, layout, &passes, &done)
        });
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::{Request, explain};
    use crate::engine::tests::{
        LINEAR_GROWTH_CAP, assert_bounded, compact_nesting, cpu_ms, on_deep_stack, ratio,
    };
    use crate::engine::types::Braces;

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
