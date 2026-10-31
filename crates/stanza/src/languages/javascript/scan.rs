use oxc_ast::AstKind;
use oxc_ast::ast::{FormalParameter, Program};
use oxc_ast_visit::Visit;
use oxc_span::{GetSpan, Span};

use crate::engine::directives::{Region, freeze, marks};
use crate::engine::model::{Comment, CommentKind, Doc};
use crate::languages::Layout;

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

struct Scanner<'d, 'a> {
    doc: &'d Doc<'d>,
    layout: Layout,
    marked: bool,
    typescript: bool,
    frames: Vec<Frame>,
    containers: Vec<Region>,
    lists: Vec<List<'a>>,
}

impl<'a> Visit<'a> for Scanner<'_, 'a> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        if let Some(list) = list_at(self.doc, &kind, self.layout) {
            self.lists.push(list);
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

    fn leave_node(&mut self, _kind: AstKind<'a>) {
        if self.marked {
            self.frames.pop();
        }
    }
}

pub struct Scan<'a> {
    pub lists: Vec<List<'a>>,
    pub frozen: Vec<Region>,
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
    };

    scanner.visit_program(program);

    let (lists, frozen) = freeze(doc, &marked, scanner.lists, &scanner.containers);
    Scan { lists, frozen }
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
