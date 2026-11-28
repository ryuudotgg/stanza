use std::collections::{HashMap, HashSet};

use oxc_allocator::Allocator;
use oxc_ast::AstKind;
use oxc_ast::ast::{BlockStatement, Program, Statement};
use oxc_ast_visit::Visit;
use oxc_span::GetSpan;

use crate::engine::directives::{Region, freeze, ignored, marks, within};
use crate::engine::doc::{document, next_token};
use crate::engine::model::{self, Comment, CommentKind, Doc, Rejection};
use crate::languages::Layout;

use super::braces::Controlled;
use super::facts::Node;
use super::lists::{List, list_at};
use super::parse::{parse, parse_source};

pub fn comments<'a>(text: &'a str, program: &Program<'a>) -> Vec<Comment<'a>> {
    let hashbang = program
        .hashbang
        .as_ref()
        .filter(|_| !program.source_type.is_typescript())
        .map(|hashbang| Comment {
            kind: CommentKind::Line,
            value: hashbang.value.as_str(),
            start: hashbang.span.start as usize,
            end: hashbang.span.end as usize,
        });

    hashbang
        .into_iter()
        .chain(program.comments.iter().map(|comment| {
            let value = comment.content_span();
            Comment {
                kind: if comment.is_line() {
                    CommentKind::Line
                } else {
                    CommentKind::Block
                },
                value: &text[value.start as usize..value.end as usize],
                start: comment.span.start as usize,
                end: comment.span.end as usize,
            }
        }))
        .collect()
}

fn container(kind: &AstKind) -> bool {
    matches!(
        kind,
        AstKind::BlockStatement(_)
            | AstKind::FunctionBody(_)
            | AstKind::StaticBlock(_)
            | AstKind::SwitchStatement(_)
            | AstKind::SwitchCase(_)
            | AstKind::ClassBody(_)
            | AstKind::TSModuleBlock(_)
    )
}

fn statement(kind: &AstKind) -> bool {
    matches!(
        kind,
        AstKind::ExpressionStatement(_)
            | AstKind::IfStatement(_)
            | AstKind::BlockStatement(_)
            | AstKind::FunctionBody(_)
            | AstKind::ForStatement(_)
            | AstKind::ForInStatement(_)
            | AstKind::ForOfStatement(_)
            | AstKind::WhileStatement(_)
            | AstKind::DoWhileStatement(_)
            | AstKind::ReturnStatement(_)
            | AstKind::ThrowStatement(_)
            | AstKind::BreakStatement(_)
            | AstKind::ContinueStatement(_)
            | AstKind::LabeledStatement(_)
            | AstKind::EmptyStatement(_)
            | AstKind::DebuggerStatement(_)
            | AstKind::WithStatement(_)
            | AstKind::SwitchStatement(_)
            | AstKind::TryStatement(_)
    )
}

fn body<'a>(kind: &AstKind<'a>) -> Option<&'a Statement<'a>> {
    match *kind {
        AstKind::ForStatement(it) => Some(&it.body),
        AstKind::ForInStatement(it) => Some(&it.body),
        AstKind::ForOfStatement(it) => Some(&it.body),
        AstKind::WhileStatement(it) => Some(&it.body),
        AstKind::DoWhileStatement(it) => Some(&it.body),
        _ => None,
    }
}

fn as_block<'a>(statement: &'a Statement<'a>) -> Option<&'a BlockStatement<'a>> {
    match statement {
        Statement::BlockStatement(block) => Some(block),
        _ => None,
    }
}

struct Owner {
    alternate: Option<u32>,
    labeled: bool,
    frozen: bool,
}

struct Scanner<'d, 'a> {
    doc: &'d Doc<'d>,
    layout: Layout,
    marked: bool,
    containers: Vec<Region>,
    lists: Vec<List<'a>>,
    commented: bool,
    owners: Vec<Owner>,
    frozen_owners: HashSet<usize>,
    bodies: Vec<Controlled<'a>>,
    chains: HashMap<usize, usize>,
}

impl<'a> Scanner<'_, 'a> {
    fn enter_owner(&mut self, kind: &AstKind) {
        let span = kind.span();
        let inherited = self.owners.last().is_some_and(|parent| {
            parent.frozen && (parent.labeled || parent.alternate == Some(span.start))
        });

        let frozen = inherited || (statement(kind) && ignored(self.doc, span.start as usize));
        if frozen {
            self.frozen_owners.insert(span.start as usize);
        }

        let alternate = match kind {
            AstKind::IfStatement(it) => it
                .alternate
                .as_ref()
                .map(|alternate| alternate.span().start),
            _ => None,
        };

        self.owners.push(Owner {
            alternate,
            labeled: matches!(kind, AstKind::LabeledStatement(_)),
            frozen,
        });
    }

    fn add_owned(&mut self, kind: &AstKind<'a>) {
        let owner = kind.span().start as usize;
        let mut add = |block: &'a BlockStatement<'a>, header: usize| {
            self.bodies.push(Controlled {
                block,
                header,
                owner,
            });
        };

        if let AstKind::IfStatement(it) = *kind {
            if let Some(consequent) = as_block(&it.consequent) {
                add(consequent, owner);
            }

            if let Some(alternate) = it.alternate.as_ref().and_then(as_block) {
                add(
                    alternate,
                    next_token(self.doc, it.consequent.span().end as usize),
                );
            }
        } else if let Some(body) = body(kind).and_then(as_block) {
            add(body, owner);
        }
    }
}

impl<'a> Visit<'a> for Scanner<'_, 'a> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        if let Some(list) = list_at(self.doc, &kind, self.layout) {
            self.lists.push(list);
        }

        if let AstKind::IfStatement(it) = kind {
            let start = it.span.start as usize;
            let head = *self.chains.entry(start).or_insert(start);
            if let Some(Statement::IfStatement(alternate)) = &it.alternate {
                self.chains.insert(alternate.span.start as usize, head);
            }
        }

        if self.commented {
            self.enter_owner(&kind);
        }

        if self.marked && container(&kind) {
            let span = kind.span();
            self.containers.push(Region {
                start: span.start as usize,
                end: span.end as usize,
            });
        }
    }

    fn leave_node(&mut self, kind: AstKind<'a>) {
        if self.commented {
            self.owners.pop();
        }

        self.add_owned(&kind);
    }
}

pub type Scan<'a> = model::Scan<Node<'a>, Controlled<'a>>;

pub fn scan<'a>(doc: &Doc, program: &'a Program<'a>, layout: Layout) -> Scan<'a> {
    let marked = marks(doc);
    let mut scanner = Scanner {
        doc,
        layout,
        marked: !marked.is_empty(),
        containers: Vec::new(),
        lists: Vec::new(),
        commented: !doc.comments.is_empty(),
        owners: Vec::new(),
        frozen_owners: HashSet::new(),
        bodies: Vec::new(),
        chains: HashMap::new(),
    };

    scanner.visit_program(program);

    let (lists, frozen) = freeze(doc, &marked, scanner.lists, &scanner.containers);
    let frozen_owners = scanner.frozen_owners;
    let eligible = scanner
        .bodies
        .iter()
        .filter(|controlled| {
            let span = controlled.block.span;
            !frozen_owners.contains(&controlled.owner)
                && !frozen_owners.contains(&(span.start as usize))
                && !within(&frozen, span.start as usize)
                && !within(&frozen, span.end as usize - 1)
        })
        .copied()
        .collect();

    Scan {
        lists,
        frozen,
        bodies: scanner.bodies,
        eligible,
        chains: scanner.chains,
    }
}

pub fn examine<R>(
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

pub fn examine_source<R>(
    path: &str,
    text: &str,
    layout: Layout,
    then: impl FnOnce(&Doc, &Scan) -> R,
) -> Result<R, Rejection> {
    let allocator = Allocator::default();
    let parsed = parse_source(&allocator, path, text)?;
    let doc = document(text, comments(text, &parsed.program));
    let scanned = scan(&doc, &parsed.program, layout);
    Ok(then(&doc, &scanned))
}

#[cfg(test)]
pub fn scanned<R>(path: &str, text: &str, check: impl FnOnce(&Doc, Scan) -> R) -> R {
    let allocator = Allocator::default();
    let parsed = parse(&allocator, path, text);
    let doc = document(text, comments(text, &parsed.program));
    let scan = scan(&doc, &parsed.program, crate::languages::FALLBACK_LAYOUT);
    check(&doc, scan)
}

#[cfg(test)]
mod tests {
    use crate::engine::doc::line_at;
    use crate::engine::model::Doc;

    use super::super::braces::{Controlled, openings};
    use super::scanned;

    fn lines(doc: &Doc, bodies: &[Controlled]) -> Vec<(usize, usize, usize)> {
        bodies
            .iter()
            .map(|controlled| {
                (
                    line_at(doc, controlled.block.span.start as usize),
                    line_at(doc, controlled.header),
                    line_at(doc, controlled.owner),
                )
            })
            .collect()
    }

    #[test]
    fn controlled_body_records_keep_frozen_openings() {
        let text = "function f() {\n  // stanza-ignore\n  if (a) { b(); }\n  if (c)\n  {\n    d();\n  }\n  else\n  {\n    e();\n  }\n}\n";
        scanned("a.ts", text, |doc, scan| {
            assert_eq!(lines(doc, &scan.bodies), [(3, 3, 3), (5, 4, 4), (9, 8, 4)]);
            assert_eq!(lines(doc, &scan.eligible), [(5, 4, 4), (9, 8, 4)]);

            let opened = |line| -> Vec<usize> {
                openings(doc, &scan.bodies, line)
                    .iter()
                    .map(|opening| opening.open)
                    .collect()
            };

            assert_eq!(opened(3), [3]);
            assert_eq!(opened(4), [5, 9]);
            assert_eq!(opened(5), [5]);
            assert!(opened(8).is_empty());
            assert_eq!(opened(9), [9]);
        });
    }
}
