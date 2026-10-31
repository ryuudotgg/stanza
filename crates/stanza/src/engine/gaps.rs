use std::ops::ControlFlow;

use super::directives::{Region, within};
use super::doc::{blank_lines, comment_index, finding, line_at};
use super::model::{Binding, Doc, Kind, LineEdits, ListKind, Reads, StatementList, Stmt};
use super::rules::{Rule, Want};
use super::types::Finding;

const JUMP_KINDS: [Kind; 4] = [Kind::Return, Kind::Throw, Kind::Continue, Kind::Break];

const USE_KINDS: [Kind; 3] = [Kind::Loop, Kind::Try, Kind::Function];

const BLOCK_KINDS: [Kind; 4] = [Kind::If, Kind::Loop, Kind::Try, Kind::Switch];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Join {
    pub name: String,
    pub reader: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ruled {
    pub rule: Rule,
    pub join: Option<Join>,
}

impl Ruled {
    fn plain(rule: Rule) -> Self {
        Ruled { rule, join: None }
    }

    pub fn want(&self) -> Want {
        self.rule.gap().expect("a gap rule names the gap it wants")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Keep,
    Frozen,
    Ruled(Ruled),
}

impl Decision {
    fn wants(&self, want: Want) -> bool {
        matches!(self, Decision::Ruled(ruled) if ruled.want() == want)
    }
}

pub struct Gap<'l, N> {
    pub prev: &'l Stmt<N>,
    pub next: &'l Stmt<N>,
    pub blank: usize,
    pub decision: Decision,
}

pub type Touches<'t> = &'t dyn Fn(usize, usize) -> bool;

fn path_name(path: &[String]) -> String {
    let mut name = path[0].clone();
    for segment in &path[1..] {
        if !segment.starts_with('[') {
            name.push('.');
        }

        name.push_str(segment);
    }

    name
}

fn first_reference<N: Reads>(doc: &Doc, stmt: &Stmt<N>, names: &[&str]) -> Option<String> {
    let mut first = None;
    stmt.node.references(doc, names, &mut |name| {
        first = Some(name.to_owned());
        ControlFlow::Break(())
    });

    first
}

fn read_by<N: Reads>(doc: &Doc, bound: &Binding, stmt: &Stmt<N>) -> Option<String> {
    match bound {
        Binding::Names(names) => {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            first_reference(doc, stmt, &names)
        }
        Binding::Path(path) => stmt.node.reads_path(doc, path).then(|| path_name(path)),
    }
}

fn join_rule<N>(prev: &Stmt<N>, next: &Stmt<N>) -> Option<Rule> {
    match next.facts.kind {
        Kind::If => Some(Rule::GuardJoin),
        Kind::Return | Kind::Switch => Some(Rule::ConsumeJoin),
        kind if USE_KINDS.contains(&kind) && prev.facts.kind == Kind::Declaration => {
            Some(Rule::UseJoin)
        }
        _ => None,
    }
}

fn declaration_join<N: Reads>(doc: &Doc, prev: &Stmt<N>, next: &Stmt<N>) -> Option<Ruled> {
    let rule = join_rule(prev, next)?;
    let reader = next.facts.word?;
    let name = read_by(doc, prev.facts.binds.as_ref()?, next)?;
    Some(Ruled {
        rule,
        join: Some(Join { name, reader }),
    })
}

fn short_body<N>(list: &StatementList<N>) -> bool {
    list.kind == ListKind::Block
        && (2..=3).contains(&list.stmts.len())
        && list.stmts.iter().all(|stmt| stmt.facts.compact)
}

type Step<N> = fn(&Doc, &StatementList<N>, &Stmt<N>, &Stmt<N>) -> Option<Ruled>;

fn ladder<N: Reads>() -> [Step<N>; 6] {
    [
        |_, list, _, _| short_body(list).then(|| Ruled::plain(Rule::ShortBody)),
        |_, _, prev, next| {
            let chained = prev.facts.guard.is_some()
                && next.facts.guard.is_some()
                && prev.facts.compact
                && next.facts.compact;

            chained.then(|| Ruled::plain(Rule::GuardChain))
        },
        |_, _, prev, next| {
            let imports = prev.facts.kind == Kind::Import && next.facts.kind == Kind::Import;
            (prev.multiline && !imports).then(|| Ruled::plain(Rule::AfterMultiline))
        },
        |_, list, prev, _| {
            let clause = list.kind == ListKind::Switch
                && prev.facts.kind == Kind::Case
                && prev.facts.case_body;

            clause.then(|| Ruled::plain(Rule::SwitchClauses))
        },
        |doc, _, prev, next| declaration_join(doc, prev, next),
        |_, _, prev, next| {
            let jumps = prev.facts.guard.is_some_and(|guard| guard.jump.is_some());
            let stepped = next.facts.kind != Kind::If && !JUMP_KINDS.contains(&next.facts.kind);
            (jumps && stepped).then(|| Ruled::plain(Rule::AfterGuard))
        },
    ]
}

pub fn matches<'a, N: Reads>(
    doc: &'a Doc,
    list: &'a StatementList<N>,
    prev: &'a Stmt<N>,
    next: &'a Stmt<N>,
) -> impl Iterator<Item = Ruled> + 'a {
    ladder::<N>()
        .into_iter()
        .filter_map(move |step| step(doc, list, prev, next))
}

pub fn decide<N: Reads>(
    doc: &Doc,
    list: &StatementList<N>,
    prev: &Stmt<N>,
    next: &Stmt<N>,
) -> Decision {
    if prev.frozen || next.frozen {
        return Decision::Frozen;
    }

    matches(doc, list, prev, next)
        .next()
        .map_or(Decision::Keep, Decision::Ruled)
}

fn may_join_run<N>(gap: &Gap<N>) -> bool {
    gap.decision == Decision::Keep
        && !gap.next.detached
        && gap
            .prev
            .facts
            .declaration
            .is_some_and(|declaration| declaration.let_like)
        && !gap.prev.multiline
        && gap.prev.facts.binds.is_some()
}

fn reference_ranks<'b, N: Reads>(
    doc: &Doc,
    block: &Stmt<N>,
    bindings: impl Iterator<Item = &'b Binding>,
) -> Vec<String> {
    let mut names: Vec<&str> = Vec::new();
    for bound in bindings {
        if let Binding::Names(bound) = bound {
            for name in bound {
                if !names.contains(&name.as_str()) {
                    names.push(name);
                }
            }
        }
    }

    let mut ranks: Vec<String> = Vec::new();
    if names.is_empty() {
        return ranks;
    }

    block.node.references(doc, &names, &mut |name| {
        if !ranks.iter().any(|ranked| ranked == name) {
            ranks.push(name.to_owned());
        }

        if ranks.len() == names.len() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });

    ranks
}

fn first_read<N: Reads>(
    doc: &Doc,
    bound: &Binding,
    block: &Stmt<N>,
    ranks: &[String],
) -> Option<String> {
    let Binding::Names(bound) = bound else {
        return read_by(doc, bound, block);
    };

    bound
        .iter()
        .filter_map(|name| Some((ranks.iter().position(|ranked| ranked == name)?, name)))
        .min_by_key(|&(rank, _)| rank)
        .map(|(_, name)| name.clone())
}

fn bound<'l, N>(gap: &Gap<'l, N>) -> &'l Binding {
    gap.prev.facts.binds.as_ref().expect("a run binds")
}

fn let_steps<N: Reads>(doc: &Doc, gaps: &mut [Gap<N>]) {
    for index in 1..gaps.len() {
        let joined = &gaps[index];
        let Decision::Ruled(decision) = &joined.decision else {
            continue;
        };

        let let_like = joined
            .prev
            .facts
            .declaration
            .is_some_and(|declaration| declaration.let_like);

        if !let_like || decision.want() != Want::None || decision.rule == Rule::ShortBody {
            continue;
        }

        let decision = decision.clone();
        let block = joined.next;

        let mut start = index;
        while start > 0 && may_join_run(&gaps[start - 1]) {
            start -= 1;
        }

        let ranks = reference_ranks(
            doc,
            block,
            gaps[start..index].iter().rev().map(|gap| bound(gap)),
        );

        let mut first = index;
        for at in (start..index).rev() {
            let Some(name) = first_read(doc, bound(&gaps[at]), block, &ranks) else {
                break;
            };

            first -= 1;
            gaps[at].decision = Decision::Ruled(Ruled {
                rule: decision.rule,
                join: decision.join.as_ref().map(|join| Join {
                    name,
                    reader: join.reader,
                }),
            });
        }

        if let Some(above) = first.checked_sub(1).map(|above| &mut gaps[above])
            && above.decision == Decision::Keep
        {
            above.decision = Decision::Ruled(Ruled::plain(Rule::LetStep));
        }
    }
}

fn bracketed_try<N: Reads>(doc: &Doc, prev: &Stmt<N>, next: &Stmt<N>) -> bool {
    prev.facts
        .operation
        .as_deref()
        .is_some_and(|operation| next.node.finally_repeats(doc, operation))
}

pub fn block_spacing<N: Reads>(path: &str, doc: &Doc, gap: &Gap<N>) -> Option<Finding> {
    let Gap {
        prev,
        next,
        blank,
        decision,
    } = gap;

    if *decision != Decision::Keep
        || *blank != 0
        || prev.multiline
        || !next.multiline
        || !BLOCK_KINDS.contains(&next.facts.kind)
    {
        return None;
    }

    if prev.facts.kind == Kind::If && next.facts.kind == Kind::If {
        return None;
    }

    if bracketed_try(doc, prev, next) {
        return None;
    }

    Some(finding(
        path,
        doc,
        next.start,
        Rule::BlockSpacing,
        Rule::BlockSpacing.message(&[]),
    ))
}

fn fix_keeps_blank<N>(gap: &Gap<N>, under_comment: bool) -> bool {
    !gap.decision.wants(Want::None) || under_comment
}

fn comment_above<N>(doc: &Doc, stmt: &Stmt<N>) -> bool {
    stmt.detached || line_at(doc, stmt.start) > stmt.start_line
}

fn gap_edits<N>(path: &str, doc: &Doc, gap: &Gap<N>, edits: &mut LineEdits) -> Option<Finding> {
    let Gap {
        prev,
        next,
        blank,
        decision,
    } = gap;

    let Decision::Ruled(decision) = decision else {
        return None;
    };

    if decision.want() == Want::None {
        if *blank == 0 || fix_keeps_blank(gap, next.detached) {
            return None;
        }

        edits
            .delete_lines
            .extend(blank_lines(doc, prev.end_line, next.start_line));
    } else {
        if *blank > 0 || next.start_line <= prev.end_line {
            return None;
        }

        edits.insert_after.insert(next.start_line - 1);
    }

    let message = match &decision.join {
        Some(join) => decision
            .rule
            .message(&[("name", &join.name), ("reader", join.reader)]),
        None => decision.rule.message(&[]),
    };

    Some(finding(path, doc, next.start, decision.rule, message))
}

fn edge_edits<N>(
    path: &str,
    doc: &Doc,
    list: &StatementList<N>,
    regions: &[Region],
    edits: &mut LineEdits,
    touches: Touches,
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
        if edge.is_some_and(|stmt| stmt.frozen) || !touches(after, before) {
            continue;
        }

        for line in blank_lines(doc, after, before) {
            let start = doc.line_starts[line - 1];
            if edits.delete_lines.contains(&line) || within(regions, start) {
                continue;
            }

            edits.delete_lines.insert(line);
            findings.push(finding(
                path,
                doc,
                start,
                Rule::EdgeBlank,
                Rule::EdgeBlank.message(&[("side", side)]),
            ));
        }
    }

    findings
}

fn separated_after_fix<N>(gap: &Gap<N>, touched: bool) -> bool {
    if gap.decision == Decision::Frozen {
        return true;
    }

    if !touched {
        return gap.blank > 0;
    }

    gap.decision.wants(Want::AtLeastOne)
        || (gap.blank > 0 && fix_keeps_blank(gap, gap.next.detached))
}

fn is_counted<N>(stmt: Option<&Stmt<N>>) -> bool {
    stmt.is_some_and(|stmt| !stmt.multiline && stmt.facts.kind != Kind::Import)
}

fn walls<N>(
    path: &str,
    doc: &Doc,
    list: &StatementList<N>,
    gaps: &[Gap<N>],
    touches: Touches,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    if list.kind == ListKind::Switch {
        return findings;
    }

    let gap_before = |index: usize| index.checked_sub(1).and_then(|before| gaps.get(before));

    let mut excluded = vec![false; list.stmts.len() + 1];

    let mut group_start = 0;
    for index in 0..=list.stmts.len() {
        let counted = is_counted(list.stmts.get(index));
        let forced_joined = gap_before(index).is_some_and(|gap| {
            !fix_keeps_blank(gap, comment_above(doc, gap.next))
                && (gap.blank == 0 || touches(gap.prev.start_line, gap.next.end_line))
        });

        if !counted || !forced_joined {
            if index - group_start >= 6 {
                excluded[group_start..index].fill(true);
            }

            group_start = index;
        }

        if !counted {
            group_start = index + 1;
        }
    }

    let mut run_start: Option<&Stmt<N>> = None;
    let mut length = 0;
    for (index, is_excluded) in excluded.into_iter().enumerate() {
        let stmt = list.stmts.get(index);
        let separated = gap_before(index).is_some_and(|gap| {
            separated_after_fix(gap, touches(gap.prev.start_line, gap.next.end_line))
        });

        let counted = is_counted(stmt) && !is_excluded;
        if !counted || separated {
            if let Some(run) = run_start
                && length >= 6
                && touches(run.start_line, list.stmts[index - 1].end_line)
            {
                findings.push(finding(
                    path,
                    doc,
                    run.start,
                    Rule::Wall,
                    Rule::Wall.message(&[]),
                ));
            }

            run_start = None;
            length = 0;
        }

        if !counted {
            continue;
        }

        run_start = run_start.or(stmt);
        length += 1;
    }

    findings
}

pub fn list_gaps<'l, N: Reads>(doc: &Doc, list: &'l StatementList<N>) -> Vec<Gap<'l, N>> {
    let mut gaps: Vec<Gap<N>> = list
        .stmts
        .windows(2)
        .map(|pair| Gap {
            prev: &pair[0],
            next: &pair[1],
            blank: blank_lines(doc, pair[0].end_line, pair[1].start_line).count(),
            decision: decide(doc, list, &pair[0], &pair[1]),
        })
        .collect();

    let_steps(doc, &mut gaps);
    gaps
}

pub struct Spacing {
    pub edits: LineEdits,
    pub findings: Vec<Finding>,
}

pub fn spacing<N: Reads>(
    path: &str,
    doc: &Doc,
    lists: &[StatementList<N>],
    regions: &[Region],
    touches: Touches,
) -> Spacing {
    let mut edits = LineEdits::default();
    let mut findings = Vec::new();
    for list in lists {
        let gaps = list_gaps(doc, list);
        for gap in &gaps {
            if touches(gap.prev.start_line, gap.next.end_line) {
                findings.extend(gap_edits(path, doc, gap, &mut edits));
                findings.extend(block_spacing(path, doc, gap));
            }
        }

        findings.extend(edge_edits(path, doc, list, regions, &mut edits, touches));
        findings.extend(walls(path, doc, list, &gaps, touches));
    }

    Spacing { edits, findings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::doc::document;
    use crate::engine::model::{Declaration, Facts};
    use crate::engine::process_file;

    #[derive(Clone, Copy)]
    struct Reader(bool);

    impl Reads for Reader {
        fn references(
            &self,
            _doc: &Doc,
            names: &[&str],
            each: &mut dyn FnMut(&str) -> ControlFlow<()>,
        ) {
            if self.0 && names.contains(&"x") {
                let _ = each("x");
            }
        }

        fn reads_path(&self, _doc: &Doc, _path: &[String]) -> bool {
            false
        }

        fn finally_repeats(&self, _doc: &Doc, _operation: &str) -> bool {
            false
        }
    }

    fn stmt(line: usize, reads: bool, facts: Facts) -> Stmt<Reader> {
        Stmt {
            node: Reader(reads),
            start: line * 10,
            end: line * 10 + 9,
            facts,
            frozen: false,
            start_line: line,
            end_line: line,
            multiline: false,
            detached: false,
        }
    }

    fn facts(kind: Kind) -> Facts {
        Facts {
            kind,
            word: None,
            binds: None,
            declaration: None,
            guard: None,
            compact: true,
            joined: None,
            case_body: false,
            operation: None,
        }
    }

    fn joined_by(word: &'static str) -> Decision {
        let declaration = stmt(
            2,
            false,
            Facts {
                declaration: Some(Declaration {
                    keyword: "const",
                    let_like: false,
                }),
                binds: Some(Binding::Names(vec!["x".to_owned()])),
                ..facts(Kind::Declaration)
            },
        );

        let reader = stmt(
            4,
            true,
            Facts {
                word: Some(word),
                ..facts(Kind::If)
            },
        );

        let list = StatementList {
            kind: ListKind::Block,
            start: 0,
            end: 100,
            open_line: Some(1),
            close_line: Some(9),
            stmts: vec![
                declaration,
                reader,
                stmt(5, false, facts(Kind::Expression)),
                stmt(6, false, facts(Kind::Expression)),
            ],
        };

        let doc = document("", Vec::new());
        decide(&doc, &list, &list.stmts[0], &list.stmts[1])
    }

    #[test]
    fn the_ladder_joins_an_if_to_the_declaration_it_reads_from_facts_alone() {
        assert_eq!(
            joined_by("if"),
            Decision::Ruled(Ruled {
                rule: Rule::GuardJoin,
                join: Some(Join {
                    name: "x".to_owned(),
                    reader: "if",
                }),
            })
        );
    }

    #[test]
    fn a_guard_word_changes_only_the_reader_the_join_names() {
        assert_eq!(
            joined_by("guard"),
            Decision::Ruled(Ruled {
                rule: Rule::GuardJoin,
                join: Some(Join {
                    name: "x".to_owned(),
                    reader: "guard",
                }),
            })
        );
    }

    fn guards(count: usize, split: bool) -> Vec<String> {
        (0..count)
            .map(|index| {
                let gap = if split && index == 3 { "\n" } else { "" };
                format!("{gap}  if (status === {}) return \"{index}\";", 401 + index)
            })
            .collect()
    }

    fn calls(count: usize) -> Vec<String> {
        (0..count)
            .map(|index| format!("  console.log({index});"))
            .collect()
    }

    fn body(lines: &[String]) -> String {
        format!("function run(status, rows) {{\n{}\n}}\n", lines.join("\n"))
    }

    fn lines(parts: &[&[String]]) -> Vec<String> {
        parts.concat()
    }

    fn line(text: &str) -> Vec<String> {
        vec![text.to_owned()]
    }

    fn wall_lines(source: &str) -> Vec<usize> {
        let result = process_file("wall.ts", source);
        assert!(!result.parse_error);
        result
            .findings
            .iter()
            .inspect(|finding| assert_eq!(finding.rule, Rule::Wall, "{finding:?}"))
            .map(|finding| finding.line)
            .collect()
    }

    #[test]
    fn walls_report_where_a_blank_line_fix_keeps_can_split_the_run() {
        let ret = line("  return \"z\";");
        let note = line("  // note");
        let chain = guards(6, false);
        let lets: Vec<String> = [
            "  let alpha = 0;",
            "  let bravo = 0;",
            "  let charlie = 0;",
            "  // note",
            "  let delta = 0;",
            "  let echo = 0;",
            "  let foxtrot = 0;",
            "  for (const row of rows) alpha += bravo + charlie + delta + echo + foxtrot + row;",
        ]
        .map(str::to_owned)
        .into();

        let mut split_calls = calls(6);
        split_calls[3] = format!("\n{}", split_calls[3]);

        let detached = [
            "  if (ready) consume(0);",
            "  if (ready) consume(1);",
            "  if (ready) consume(2);",
            "  // section",
            "",
            "  if (ready) consume(3);",
            "  if (ready) consume(4);",
            "  if (ready) consume(5);",
            "  consume(6);",
            "  consume(7);",
            "  consume(8);",
        ]
        .map(str::to_owned);

        let mixed = [
            "  let alpha = 0;",
            "  let bravo = 0;",
            "  let charlie = 0;",
            "  for (const row of rows) alpha += bravo + charlie + row;",
            "  console.log(0);",
            "  console.log(1);",
        ]
        .map(str::to_owned);

        let blank = line("");
        let cases: Vec<(&str, Vec<String>, Vec<usize>)> = vec![
            ("six calls", calls(6), vec![2]),
            ("six calls split", split_calls, vec![]),
            (
                "five guards and a return",
                lines(&[&guards(5, false), &ret]),
                vec![2],
            ),
            ("use-joined lets and calls", mixed.into(), vec![2]),
            (
                "calls after a forced group",
                lines(&[&chain, &blank, &calls(6)]),
                vec![9],
            ),
            (
                "calls before a forced group",
                lines(&[&calls(6), &blank, &chain]),
                vec![2],
            ),
            (
                "calls right after a forced group",
                lines(&[&chain, &ret, &calls(6)]),
                vec![8],
            ),
            (
                "calls right before a forced group",
                lines(&[&calls(6), &chain]),
                vec![2],
            ),
            (
                "calls on both sides of a forced group",
                lines(&[&calls(6), &chain, &blank, &calls(6)]),
                vec![2, 15],
            ),
            (
                "detached comment in a forced group",
                detached.into(),
                vec![7],
            ),
            (
                "frozen calls beside a forced group",
                lines(&[&chain, &blank, &line("  // stanza-ignore"), &calls(7)]),
                vec![11],
            ),
            ("six guards and a return", lines(&[&chain, &ret]), vec![]),
            (
                "a comment above guard 2 of eight",
                lines(&[&chain[..1], &note, &guards(8, false)[1..], &ret]),
                vec![],
            ),
            ("a comment above the fourth of six lets", lets, vec![2]),
            (
                "a comment above the fourth of six calls",
                lines(&[&calls(6)[..3], &note, &calls(6)[3..]]),
                vec![2],
            ),
            (
                "a comment with a blank line below it in a guard chain",
                lines(&[&chain[..3], &note, &blank, &chain[3..], &ret]),
                vec![],
            ),
        ];

        let commented = (1..=5).map(|above| {
            (
                "a comment inside a guard chain",
                lines(&[&chain[..above], &note, &chain[above..], &ret]),
                vec![2],
            )
        });

        for (name, statements, expected) in cases.into_iter().chain(commented) {
            assert_eq!(wall_lines(&body(&statements)), expected, "{name}");
        }
    }

    #[test]
    fn a_let_run_takes_the_name_its_block_reads_first() {
        let source = "function f(x) {\n  run();\n  let [a, b] = x;\n\n  let c = 0;\n\n  for (const r of x) c += b + a + r;\n}\n";
        let findings: Vec<(usize, Rule, String)> = process_file("a.ts", source)
            .findings
            .into_iter()
            .map(|finding| (finding.line, finding.rule, finding.message))
            .collect();

        assert_eq!(
            findings,
            [
                (3, Rule::LetStep, Rule::LetStep.message(&[])),
                (
                    5,
                    Rule::UseJoin,
                    "keep `b` next to the `for` that reads it (remove the blank line)".to_owned()
                ),
                (
                    7,
                    Rule::UseJoin,
                    "keep `c` next to the `for` that reads it (remove the blank line)".to_owned()
                ),
            ]
        );
    }
}
