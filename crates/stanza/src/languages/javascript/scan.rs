use std::collections::HashSet;

use oxc_ast::AstKind;
use oxc_ast::ast::{BlockStatement, FormalParameter, Program, Statement};
use oxc_ast_visit::Visit;
use oxc_span::{GetSpan, Span};

use crate::engine::directives::{Region, freeze, ignored, marks, within};
use crate::engine::doc::next_token;
use crate::engine::model::{Comment, CommentKind, Doc};
use crate::languages::Layout;

use super::braces::Controlled;
use super::lists::{List, list_at, program_span};

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

fn bare_pattern_span(param: &FormalParameter) -> Option<Span> {
    if param.has_modifier() || param.initializer.is_some() || param.optional {
        return None;
    }

    let pattern = param.pattern.span();
    Some(
        param
            .type_annotation
            .as_ref()
            .map_or(pattern, |annotation| pattern.merge(annotation.span)),
    )
}

fn estree_span(kind: &AstKind, typescript: bool) -> Span {
    match kind {
        AstKind::Program(program) => program_span(program),
        // TS-ESTree makes a plain parameter its pattern, so its decorators fall outside its span.
        AstKind::FormalParameter(param) if typescript => {
            bare_pattern_span(param).unwrap_or(param.span)
        }
        _ => kind.span(),
    }
}

struct Frame {
    span: Span,
    escaped: bool,
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

fn block<'a>(statement: &'a Statement<'a>) -> Option<&'a BlockStatement<'a>> {
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

struct Candidate<'a> {
    controlled: Controlled<'a>,
    owner: usize,
}

struct Scanner<'d, 'a> {
    doc: &'d Doc<'d>,
    layout: Layout,
    marked: bool,
    typescript: bool,
    frames: Vec<Frame>,
    containers: Vec<Region>,
    lists: Vec<List<'a>>,
    commented: bool,
    owners: Vec<Owner>,
    frozen_owners: HashSet<usize>,
    candidates: Vec<Candidate<'a>>,
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
            self.candidates.push(Candidate {
                controlled: Controlled { block, header },
                owner,
            });
        };

        if let AstKind::IfStatement(it) = *kind {
            if let Some(consequent) = block(&it.consequent) {
                add(consequent, owner);
            }

            if let Some(alternate) = it.alternate.as_ref().and_then(block) {
                add(
                    alternate,
                    next_token(self.doc, it.consequent.span().end as usize),
                );
            }
        } else if let Some(body) = body(kind).and_then(block) {
            add(body, owner);
        }
    }
}

impl<'a> Visit<'a> for Scanner<'_, 'a> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        if let Some(list) = list_at(self.doc, &kind, self.layout) {
            self.lists.push(list);
        }

        if self.commented {
            self.enter_owner(&kind);
        }

        if !self.marked {
            return;
        }

        let span = estree_span(&kind, self.typescript);

        let escaped = self.frames.last().is_some_and(|parent| {
            parent.escaped || span.start < parent.span.start || parent.span.end < span.end
        });

        if !escaped && container(&kind) {
            self.containers.push(Region {
                start: span.start as usize,
                end: span.end as usize,
            });
        }

        self.frames.push(Frame { span, escaped });
    }

    fn leave_node(&mut self, kind: AstKind<'a>) {
        if self.marked {
            self.frames.pop();
        }

        if self.commented {
            self.owners.pop();
        }

        self.add_owned(&kind);
    }
}

pub struct Scan<'a> {
    pub lists: Vec<List<'a>>,
    pub frozen: Vec<Region>,
    pub blocks: Vec<Controlled<'a>>,
}

pub fn scan<'a>(doc: &Doc, program: &'a Program<'a>, layout: Layout) -> Scan<'a> {
    let marked = marks(doc);
    let mut scanner = Scanner {
        doc,
        layout,
        marked: !marked.is_empty(),
        typescript: program.source_type.is_typescript(),
        frames: Vec::new(),
        containers: Vec::new(),
        lists: Vec::new(),
        commented: !doc.comments.is_empty(),
        owners: Vec::new(),
        frozen_owners: HashSet::new(),
        candidates: Vec::new(),
    };

    scanner.visit_program(program);

    let (lists, frozen) = freeze(doc, &marked, scanner.lists, &scanner.containers);
    let frozen_owners = scanner.frozen_owners;
    let blocks = scanner
        .candidates
        .into_iter()
        .filter(|candidate| {
            let span = candidate.controlled.block.span;
            !frozen_owners.contains(&candidate.owner)
                && !frozen_owners.contains(&(span.start as usize))
                && !within(&frozen, span.start as usize)
                && !within(&frozen, span.end as usize - 1)
        })
        .map(|candidate| candidate.controlled)
        .collect();

    Scan {
        lists,
        frozen,
        blocks,
    }
}

#[cfg(test)]
pub fn scanned<R>(path: &str, text: &str, check: impl FnOnce(&Doc, Scan) -> R) -> R {
    use crate::engine::doc::document;

    let allocator = oxc_allocator::Allocator::default();
    let parsed = super::parse::parse(&allocator, path, text);
    let doc = document(text, comments(text, &parsed.program));
    let scan = scan(&doc, &parsed.program, crate::languages::FALLBACK_LAYOUT);
    check(&doc, scan)
}
