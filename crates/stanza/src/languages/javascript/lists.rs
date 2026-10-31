use oxc_ast::AstKind;
use oxc_ast::ast::{Declaration, Directive, ExportDefaultDeclarationKind, Program, Statement};
use oxc_span::{GetSpan, Span};

use crate::engine::attach::attach;
use crate::engine::doc::{line_at, next_token};
use crate::engine::model::{Doc, Facts, Item, Kind, ListKind, StatementList};

const STATEMENT: Facts = Facts {
    kind: Kind::Other,
    case_body: false,
};

fn item(span: Span, facts: Facts) -> Item {
    Item {
        start: span.start as usize,
        end: span.end as usize,
        facts,
    }
}

fn statements(directives: &[Directive], body: &[Statement]) -> Vec<Item> {
    directives
        .iter()
        .map(|directive| item(directive.span, STATEMENT))
        .chain(body.iter().map(|stmt| item(stmt.span(), STATEMENT)))
        .collect()
}

fn class_decorator_start(stmt: &Statement) -> Option<u32> {
    let class = match stmt {
        Statement::ExportDeclaration(export) => match &export.declaration {
            Declaration::ClassDeclaration(class) => class,
            _ => return None,
        },
        Statement::ExportDefaultDeclaration(export) => match &export.declaration {
            ExportDefaultDeclarationKind::ClassDeclaration(class) => class,
            _ => return None,
        },
        _ => return None,
    };

    class
        .decorators
        .first()
        .map(|decorator| decorator.span.start)
}

pub fn program_span(program: &Program) -> Span {
    if !program.source_type.is_typescript() {
        return program.span;
    }

    let start = match (program.directives.first(), program.body.first()) {
        (Some(directive), _) => directive.span.start,
        (None, Some(first)) => {
            let start = first.span().start;
            class_decorator_start(first).map_or(start, |decorator| decorator.min(start))
        }
        (None, None) => program.span.end,
    };

    Span::new(start, program.span.end)
}

fn block(doc: &Doc, start: usize, end: usize, items: &[Item]) -> StatementList {
    StatementList {
        kind: ListKind::Block,
        start,
        end,
        open_line: Some(line_at(doc, start)),
        close_line: Some(line_at(doc, end - 1)),
        stmts: attach(doc, items, start + 1, end - 1),
    }
}

pub fn list_at(doc: &Doc, kind: &AstKind) -> Option<StatementList> {
    match kind {
        AstKind::Program(program) => {
            let span = program_span(program);
            Some(StatementList {
                kind: ListKind::Module,
                start: span.start as usize,
                end: span.end as usize,
                open_line: None,
                close_line: None,
                stmts: attach(
                    doc,
                    &statements(&program.directives, &program.body),
                    span.start as usize,
                    span.end as usize,
                ),
            })
        }
        AstKind::BlockStatement(node) => Some(block(
            doc,
            node.span.start as usize,
            node.span.end as usize,
            &statements(&[], &node.body),
        )),
        AstKind::FunctionBody(node) => Some(block(
            doc,
            node.span.start as usize,
            node.span.end as usize,
            &statements(&node.directives, &node.statements),
        )),
        AstKind::StaticBlock(node) => Some(block(
            doc,
            next_token(doc, node.span.start as usize + "static".len()),
            node.span.end as usize,
            &statements(&[], &node.body),
        )),
        AstKind::SwitchStatement(node) => {
            let items: Vec<Item> = node
                .cases
                .iter()
                .map(|case| {
                    let facts = Facts {
                        kind: Kind::Case,
                        case_body: !case.consequent.is_empty(),
                    };

                    item(case.span, facts)
                })
                .collect();

            Some(StatementList {
                kind: ListKind::Switch,
                start: node.span.start as usize,
                end: node.span.end as usize,
                open_line: None,
                close_line: None,
                stmts: attach(
                    doc,
                    &items,
                    node.discriminant.span().end as usize,
                    node.span.end as usize,
                ),
            })
        }
        AstKind::SwitchCase(node) => Some(StatementList {
            kind: ListKind::Block,
            start: node.span.start as usize,
            end: node.span.end as usize,
            open_line: None,
            close_line: None,
            stmts: attach(
                doc,
                &statements(&[], &node.consequent),
                node.test
                    .as_ref()
                    .map_or(node.span.start, |test| test.span().end) as usize,
                node.span.end as usize,
            ),
        }),
        _ => None,
    }
}
