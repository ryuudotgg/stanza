use oxc_ast::AstKind;
use oxc_ast::ast::{Declaration, Directive, ExportDefaultDeclarationKind, Statement};
use oxc_span::GetSpan;

use crate::engine::attach::attach;
use crate::engine::doc::{line_at, next_token};
use crate::engine::model::{Doc, Item, ListKind, StatementList};
use crate::languages::Layout;

use super::facts::{Node, facts};

pub type List<'a> = StatementList<Node<'a>>;

fn item<'a>(doc: &Doc, node: Node<'a>, layout: Layout) -> Item<Node<'a>> {
    let span = node.span();
    let start = match node {
        Node::Statement(stmt) => statement_start(stmt),
        _ => span.start,
    };

    Item {
        node,
        start: start as usize,
        end: span.end as usize,
        facts: facts(doc, node, layout),
    }
}

fn statements<'a, 'd>(
    doc: &'d Doc,
    directives: &'a [Directive<'a>],
    body: &'a [Statement<'a>],
    layout: Layout,
) -> impl Iterator<Item = Item<Node<'a>>> + use<'a, 'd> {
    directives
        .iter()
        .map(move |directive| item(doc, Node::Directive(directive), layout))
        .chain(
            body.iter()
                .map(move |stmt| item(doc, Node::Statement(stmt), layout)),
        )
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

fn statement_start(stmt: &Statement) -> u32 {
    let start = stmt.span().start;
    class_decorator_start(stmt).map_or(start, |decorator| decorator.min(start))
}

fn block<'a>(
    doc: &Doc,
    start: usize,
    end: usize,
    items: impl Iterator<Item = Item<Node<'a>>>,
) -> List<'a> {
    StatementList {
        kind: ListKind::Block,
        start,
        end,
        open_line: Some(line_at(doc, start)),
        close_line: Some(line_at(doc, end - 1)),
        stmts: attach(doc, items, start + 1, end - 1),
    }
}

pub fn list_at<'a>(doc: &Doc, kind: &AstKind<'a>, layout: Layout) -> Option<List<'a>> {
    match kind {
        AstKind::Program(program) => Some(StatementList {
            kind: ListKind::Module,
            start: program.span.start as usize,
            end: program.span.end as usize,
            open_line: None,
            close_line: None,
            stmts: attach(
                doc,
                statements(doc, &program.directives, &program.body, layout),
                program.span.start as usize,
                program.span.end as usize,
            ),
        }),
        AstKind::BlockStatement(node) => Some(block(
            doc,
            node.span.start as usize,
            node.span.end as usize,
            statements(doc, &[], &node.body, layout),
        )),
        AstKind::FunctionBody(node) => Some(block(
            doc,
            node.span.start as usize,
            node.span.end as usize,
            statements(doc, &node.directives, &node.statements, layout),
        )),
        AstKind::StaticBlock(node) => Some(block(
            doc,
            next_token(doc, node.span.start as usize + "static".len()),
            node.span.end as usize,
            statements(doc, &[], &node.body, layout),
        )),
        AstKind::SwitchStatement(node) => Some(StatementList {
            kind: ListKind::Switch,
            start: node.span.start as usize,
            end: node.span.end as usize,
            open_line: None,
            close_line: None,
            stmts: attach(
                doc,
                node.cases
                    .iter()
                    .map(|case| item(doc, Node::Case(case), layout)),
                node.discriminant.span().end as usize,
                node.span.end as usize,
            ),
        }),
        AstKind::SwitchCase(node) => Some(StatementList {
            kind: ListKind::Block,
            start: node.span.start as usize,
            end: node.span.end as usize,
            open_line: None,
            close_line: None,
            stmts: attach(
                doc,
                statements(doc, &[], &node.consequent, layout),
                node.test
                    .as_ref()
                    .map_or(node.span.start, |test| test.span().end) as usize,
                node.span.end as usize,
            ),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::process_file;
    use crate::engine::rules::Rule;
    use crate::engine::types::Mode;
    use crate::files::Changed;
    use crate::languages::FALLBACK_LAYOUT;

    fn checked(text: &str) -> Vec<(usize, usize, Rule)> {
        process_file("a.ts", text, Mode::Check, false, FALLBACK_LAYOUT, None)
            .findings
            .into_iter()
            .map(|finding| (finding.line, finding.col, finding.rule))
            .collect()
    }

    fn fixed(text: &str) -> Option<String> {
        process_file("a.ts", text, Mode::Fix, false, FALLBACK_LAYOUT, None)
            .fixed
            .map(|fixed| fixed.text)
    }

    #[test]
    fn a_decorated_export_starts_at_its_decorator() {
        for export in ["export class", "export default class"] {
            let text = format!("f(\n  1,\n);\n@dec\n{export} A {{}}\nx();\n");

            assert_eq!(
                fixed(&text),
                Some(format!("f(\n  1,\n);\n\n@dec\n{export} A {{}}\n\nx();\n")),
                "{export}"
            );
            assert_eq!(
                checked(&text),
                [(4, 1, Rule::AfterMultiline), (6, 1, Rule::AfterMultiline)],
                "{export}"
            );
        }
    }

    #[test]
    fn an_ignore_above_a_decorated_export_holds_its_gaps() {
        for export in ["export class", "export default class"] {
            let text = format!(
                "before();\n// stanza-ignore\n@dec\n{export} A {{\n  m() {{}}\n}}\nafter();\n"
            );

            assert_eq!(fixed(&text), None, "{export}");
            assert_eq!(checked(&text), [], "{export}");
        }
    }

    #[test]
    fn a_decorated_export_with_the_decorator_after_export_keeps_its_start() {
        for export in ["export @dec class", "export default @dec class"] {
            let text = format!("f(\n  1,\n);\n{export} A {{}}\nx();\n");

            assert_eq!(
                fixed(&text),
                Some(format!("f(\n  1,\n);\n\n{export} A {{}}\nx();\n")),
                "{export}"
            );
            assert_eq!(checked(&text), [(4, 1, Rule::AfterMultiline)], "{export}");
        }
    }

    #[test]
    fn a_changed_leading_comment_scopes_the_module_in_ts_and_js() {
        for path in ["h.ts", "h.js"] {
            let changed = Changed {
                lines: [1].into(),
                ..Changed::default()
            };

            let findings: Vec<_> = process_file(
                path,
                "// changed header\nconst a = f();\n\nif (a) g();\n",
                Mode::Check,
                false,
                FALLBACK_LAYOUT,
                Some(changed),
            )
            .findings
            .into_iter()
            .map(|finding| (finding.line, finding.col, finding.rule))
            .collect();

            assert_eq!(findings, [(4, 1, Rule::GuardJoin)], "{path}");
        }
    }
}
