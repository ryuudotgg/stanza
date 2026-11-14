use std::num::NonZeroUsize;

use oxc_allocator::Allocator;

use crate::languages::Layout;
use crate::languages::javascript::braces::{Opening, Verdict, brace_edits, openings, verdict};
use crate::languages::javascript::config::brace_decisions;
use crate::languages::javascript::config::layers::Setting;
use crate::languages::javascript::config::width::{Width, WidthSource, formatter_width};
use crate::languages::javascript::facts::Node;
use crate::languages::javascript::lists::List;
use crate::languages::javascript::parse::parse_source;
use crate::languages::javascript::scan::{Scan, comments, scan};
use crate::paths::{dirname, extname};
use crate::text::js_trim;

use super::directives::within;
use super::doc::{document, line_at, position};
use super::edits::{OffsetEdit, OffsetMap, apply_offsets, back_through};
use super::examined;
use super::gaps::{Decision, Gap, Ruled, block_spacing, list_gaps, matches};
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
    width: &'t Width,
    display: &'t dyn Fn(&str) -> String,
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

    fn excerpt(&self, first: usize, last: usize) -> String {
        let label = if first == last {
            first.to_string()
        } else {
            format!("{first}-{last}")
        };

        let width = LABEL - 2;
        format!(
            "  {label:<width$}{}",
            js_trim(self.original.lines[first - 1])
        )
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

fn explain_gap(trace: &Traced, list: &List, gap: &Gap<Node>) -> Vec<String> {
    let Gap {
        prev,
        next,
        decision,
        ..
    } = gap;

    let mut lines = vec![
        format!(
            "gap above line {}, {}",
            trace.at(trace.code_line(next)),
            plural(gap.blank, "blank line")
        ),
        trace.excerpt(trace.at(trace.code_line(prev)), trace.at(prev.end_line)),
        trace.excerpt(
            trace.at(trace.code_line(next)),
            trace.at(trace.code_line(next)),
        ),
    ];

    let first = trace.original_line(list.start);
    let last = trace.original_line(list.end - 1);
    let unbraced: Vec<usize> = trace
        .unbraced
        .iter()
        .copied()
        .filter(|line| (first..=last).contains(line))
        .collect();

    if !unbraced.is_empty() {
        let note = format!(
            "decided after --fix removes the braces on {}",
            line_list(&unbraced)
        );

        lines.push(field("note", &note));
    }

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

fn config_holds(request: &Request) -> Vec<String> {
    match request.braces {
        Some(Braces::Off) => return vec!["--no-braces turns the rule off".to_owned()],
        Some(Braces::On) => return Vec::new(),
        None => {}
    }

    match brace_decisions(&dirname(request.path), Some(extname(request.path))) {
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
        Err(_) => vec![
            "reading the lint config failed, so stanza keeps them: RangeError: Maximum call stack size exceeded.".to_owned(),
        ],
    }
}

fn explain_block(trace: &Traced, block: &Opening, config: &[String]) -> Vec<String> {
    let Opening {
        open, close, inner, ..
    } = *block;

    let mut lines = vec![if open == close {
        format!("braced body on line {open}")
    } else {
        format!("braced body, lines {open} to {close}")
    }];

    if let Some((first, last)) = inner {
        lines.push(trace.excerpt(first, last));
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
    let reason = match verdict(
        trace.doc,
        &trace.scanned.owned,
        &trace.scanned.blocks,
        offset,
        &at,
    ) {
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

pub type Line = Result<Explained, String>;

enum Pass {
    Settled(Vec<Line>),
    Unbraced(Vec<OffsetEdit>),
}

type Done<'d> = dyn Fn(&Doc, &Scan, &Passes) -> Vec<Line> + 'd;

fn settle(doc: &Doc, scanned: &Scan, layout: Layout, passes: &Passes, done: &Done) -> Pass {
    let edits = brace_edits(doc, &scanned.blocks, false, layout).edits;
    if edits.is_empty() {
        Pass::Settled(done(doc, scanned, passes))
    } else {
        Pass::Unbraced(edits)
    }
}

struct Gaps<'g> {
    found: Vec<(&'g List<'g>, Gap<'g, Node<'g>>)>,
    by_line: Vec<(usize, usize)>,
}

impl Gaps<'_> {
    fn ending_on(&self, line: usize) -> impl Iterator<Item = &(&List<'_>, Gap<'_, Node<'_>>)> {
        let from = self.by_line.partition_point(|&(at, _)| at < line);
        self.by_line[from..]
            .iter()
            .take_while(move |&&(at, _)| at == line)
            .map(|&(_, index)| &self.found[index])
    }
}

fn gaps_by_line<'g>(trace: &Traced<'g>) -> Gaps<'g> {
    let found: Vec<_> = trace
        .scanned
        .lists
        .iter()
        .flat_map(|list| {
            list_gaps(trace.doc, list)
                .into_iter()
                .map(move |gap| (list, gap))
        })
        .collect();

    let mut by_line = Vec::with_capacity(found.len());
    for (index, (_, gap)) in found.iter().enumerate() {
        let code = trace.at(trace.code_line(gap.next));
        let start = trace.at(gap.next.start_line);
        by_line.push((code, index));
        if start != code {
            by_line.push((start, index));
        }
    }

    by_line.sort_by_key(|&(line, _)| line);
    Gaps { found, by_line }
}

fn describe(
    trace: &Traced,
    gaps: &Gaps,
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
        let noun = if line_count == 1 { "line" } else { "lines" };
        return Err(format!("{path} has {line_count} {noun}, not {line}"));
    }

    let mut sections: Vec<Vec<String>> = Vec::new();
    for (list, gap) in gaps.ending_on(line) {
        sections.push(explain_gap(trace, list, gap));
    }

    for block in openings(original, &trace.first.owned, line) {
        sections.push(explain_block(trace, &block, config));
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
            &format!("could not read the line width from {}", files.join(", ")),
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
    let done = |doc: &Doc, scanned: &Scan, passes: &Passes| {
        let traced = Traced {
            original: &original,
            first: &first,
            doc,
            scanned,
            maps: &passes.maps,
            unbraced: &passes.unbraced,
            width: &width,
            display: request.display,
        };

        let gaps = gaps_by_line(&traced);
        lines
            .iter()
            .map(|&line| describe(&traced, &gaps, request, line, &config))
            .collect()
    };

    let mut passes = Passes::default();
    if !config.is_empty() {
        return Ok(done(&original, &first, &passes));
    }

    let mut current = original.text.to_owned();
    let mut pass = settle(&original, &first, layout, &passes, &done);
    loop {
        let edits = match pass {
            Pass::Settled(explained) => return Ok(explained),
            Pass::Unbraced(edits) => edits,
        };

        for opening in edits.iter().step_by(2) {
            let line = line_at(&original, back_through(&passes.maps, opening.start));
            passes.unbraced.push(line);
        }

        passes.maps.push(OffsetMap::new(&edits));
        current = apply_offsets(&current, &edits);
        pass = examined(path, &current, layout, |doc, scanned, _| {
            settle(doc, scanned, layout, &passes, &done)
        });
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::{Request, explain};
    use crate::engine::tests::{LINEAR_GROWTH_CAP, assert_bounded, cpu_ms, ratio};
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
