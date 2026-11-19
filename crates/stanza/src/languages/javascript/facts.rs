use std::ops::ControlFlow;

use oxc_ast::ast::{
    ArrowFunctionExpression, AssignmentTarget, BlockStatement, CallExpression, CatchClause,
    ChainElement, ComputedMemberExpression, Declaration, Directive, ExportDefaultDeclarationKind,
    Expression, ExpressionStatement, ForInStatement, ForOfStatement, ForStatement, Function,
    FunctionBody, FunctionType, IfStatement, JSXOpeningElement, MemberExpression,
    PrivateFieldExpression, SimpleAssignmentTarget, Statement, StaticBlock, StaticMemberExpression,
    SwitchCase, SwitchStatement, TSClassImplements, TSInterfaceHeritage, TSTypeName,
    VariableDeclarationKind,
};
use oxc_ast_visit::Visit;
use oxc_ast_visit::walk::{
    walk_block_statement, walk_expression, walk_for_in_statement, walk_for_of_statement,
    walk_for_statement, walk_function, walk_function_body, walk_jsx_opening_element,
    walk_statement, walk_static_block, walk_switch_statement, walk_ts_class_implements,
    walk_ts_interface_heritage,
};
use oxc_span::{GetSpan, Span};
use oxc_syntax::scope::ScopeFlags;

use crate::engine::doc::{columns, comment_index, line_at, source};
use crate::engine::model::{
    Binding, Declaration as Declared, Doc, Facts, Guard, Joined, Kind, Reads,
};
use crate::languages::Layout;
use crate::text::{is_js_whitespace, js_trim};

use super::ast::{
    Inner, body_declared, catch_declared, declaration_names, for_declared, jsx_element_path,
    left_declared, params_declared, references, root_function, switch_declared, target_names,
    walk_inner,
};

#[derive(Clone, Copy, Debug)]
pub enum Node<'a> {
    Directive(&'a Directive<'a>),
    Statement(&'a Statement<'a>),
    Case(&'a SwitchCase<'a>),
}

impl Node<'_> {
    pub fn span(self) -> Span {
        match self {
            Node::Directive(directive) => directive.span,
            Node::Statement(stmt) => stmt.span(),
            Node::Case(case) => case.span,
        }
    }
}

fn exported(node: Node) -> Inner {
    let stmt = match node {
        Node::Directive(directive) => return Inner::Directive(directive),
        Node::Case(case) => return Inner::Case(case),
        Node::Statement(stmt) => stmt,
    };

    match stmt {
        Statement::ExportDeclaration(export) => Inner::Declaration(&export.declaration),
        Statement::ExportDefaultDeclaration(export) => match &export.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(function)
                if declared_function(function) =>
            {
                Inner::Function(function)
            }
            _ => Inner::Statement(stmt),
        },
        _ => Inner::Statement(stmt),
    }
}

fn declared_function(function: &Function) -> bool {
    function.r#type == FunctionType::FunctionDeclaration
}

fn declaration_kind(declaration: &Declaration) -> Kind {
    match declaration {
        Declaration::VariableDeclaration(_) => Kind::Declaration,
        Declaration::FunctionDeclaration(function) if declared_function(function) => Kind::Function,
        _ => Kind::Other,
    }
}

fn kind_of(inner: Inner) -> Kind {
    let stmt = match inner {
        Inner::Directive(_) => return Kind::Other,
        Inner::Case(_) => return Kind::Case,
        Inner::Function(_) => return Kind::Function,
        Inner::Declaration(declaration) => return declaration_kind(declaration),
        Inner::Statement(stmt) => stmt,
    };

    match stmt {
        Statement::ImportDeclaration(_)
        | Statement::ExportAllDeclaration(_)
        | Statement::ExportFromDeclaration(_) => Kind::Import,
        Statement::VariableDeclaration(_) => Kind::Declaration,
        Statement::IfStatement(_) => Kind::If,
        Statement::ForStatement(_)
        | Statement::ForInStatement(_)
        | Statement::ForOfStatement(_)
        | Statement::WhileStatement(_)
        | Statement::DoWhileStatement(_) => Kind::Loop,
        Statement::SwitchStatement(_) => Kind::Switch,
        Statement::TryStatement(_) => Kind::Try,
        Statement::FunctionDeclaration(function) if declared_function(function) => Kind::Function,
        Statement::ReturnStatement(_) => Kind::Return,
        Statement::ThrowStatement(_) => Kind::Throw,
        Statement::BreakStatement(_) => Kind::Break,
        Statement::ContinueStatement(_) => Kind::Continue,
        _ => Kind::Other,
    }
}

fn word_of(inner: Inner) -> Option<&'static str> {
    let stmt = match inner {
        Inner::Function(_) => return Some("function"),
        Inner::Declaration(Declaration::FunctionDeclaration(function))
            if declared_function(function) =>
        {
            return Some("function");
        }
        Inner::Statement(stmt) => stmt,
        _ => return None,
    };

    Some(match stmt {
        Statement::IfStatement(_) => "if",
        Statement::ReturnStatement(_) => "return",
        Statement::SwitchStatement(_) => "switch",
        Statement::ForStatement(_)
        | Statement::ForInStatement(_)
        | Statement::ForOfStatement(_) => "for",
        Statement::WhileStatement(_) => "while",
        Statement::DoWhileStatement(_) => "do",
        Statement::TryStatement(_) => "try",
        Statement::FunctionDeclaration(function) if declared_function(function) => "function",
        _ => return None,
    })
}

fn jump(stmt: &Statement) -> Option<&'static str> {
    match stmt {
        Statement::ReturnStatement(_) => Some("return"),
        Statement::ThrowStatement(_) => Some("throw"),
        Statement::ContinueStatement(_) => Some("continue"),
        Statement::BreakStatement(_) => Some("break"),
        _ => None,
    }
}

fn guard_of(node: Node) -> Option<Guard> {
    let Node::Statement(Statement::IfStatement(stmt)) = node else {
        return None;
    };

    if stmt.alternate.is_some() {
        return None;
    }

    match &stmt.consequent {
        Statement::BlockStatement(block) if block.body.len() != 1 => None,
        Statement::BlockStatement(block) => Some(Guard {
            jump: jump(&block.body[0]),
        }),
        consequent => Some(Guard {
            jump: jump(consequent),
        }),
    }
}

#[derive(Clone, Copy)]
enum Member<'b, 'a> {
    Static(&'b StaticMemberExpression<'a>),
    Computed(&'b ComputedMemberExpression<'a>),
    Private(&'b PrivateFieldExpression<'a>),
}

impl<'b, 'a> Member<'b, 'a> {
    fn of(member: &'b MemberExpression<'a>) -> Self {
        match member {
            MemberExpression::StaticMemberExpression(member) => Member::Static(member),
            MemberExpression::ComputedMemberExpression(member) => Member::Computed(member),
            MemberExpression::PrivateFieldExpression(member) => Member::Private(member),
        }
    }

    fn object(self) -> &'b Expression<'a> {
        match self {
            Member::Static(member) => &member.object,
            Member::Computed(member) => &member.object,
            Member::Private(member) => &member.object,
        }
    }

    fn segment(self, doc: &Doc) -> String {
        match self {
            Member::Static(member) => member.property.name.as_str().to_owned(),
            Member::Computed(member) => {
                let span = member.expression.span();
                format!("[{}]", source(doc, span.start as usize, span.end as usize))
            }
            Member::Private(member) => format!("#{}", member.field.name.as_str()),
        }
    }
}

#[derive(Clone, Copy)]
enum Base<'b, 'a> {
    Expression(&'b Expression<'a>),
    Call(&'b CallExpression<'a>),
}

enum Unwrapped<'b, 'a> {
    Member(Member<'b, 'a>),
    Base(Base<'b, 'a>),
}

fn unwrap_path<'b, 'a>(expression: &'b Expression<'a>) -> Unwrapped<'b, 'a> {
    let mut current = expression;
    loop {
        current = match current {
            Expression::ChainExpression(chain) => match &chain.expression {
                ChainElement::CallExpression(call) => return Unwrapped::Base(Base::Call(call)),
                ChainElement::TSNonNullExpression(inner) => &inner.expression,
                ChainElement::StaticMemberExpression(member) => {
                    return Unwrapped::Member(Member::Static(member));
                }
                ChainElement::ComputedMemberExpression(member) => {
                    return Unwrapped::Member(Member::Computed(member));
                }
                ChainElement::PrivateFieldExpression(member) => {
                    return Unwrapped::Member(Member::Private(member));
                }
            },
            Expression::TSNonNullExpression(inner) => &inner.expression,
            Expression::TSAsExpression(inner) => &inner.expression,
            Expression::TSSatisfiesExpression(inner) => &inner.expression,
            Expression::TSTypeAssertion(inner) => &inner.expression,
            Expression::ParenthesizedExpression(inner) => &inner.expression,
            _ => {
                return match current.as_member_expression() {
                    Some(member) => Unwrapped::Member(Member::of(member)),
                    None => Unwrapped::Base(Base::Expression(current)),
                };
            }
        };
    }
}

fn spine<'b, 'a>(outer: Member<'b, 'a>) -> (Vec<Member<'b, 'a>>, Base<'b, 'a>) {
    let mut members = vec![outer];
    let mut base = unwrap_path(outer.object());
    while let Unwrapped::Member(member) = base {
        members.push(member);
        base = unwrap_path(member.object());
    }

    let Unwrapped::Base(base) = base else {
        unreachable!("the loop ends on a base");
    };

    (members, base)
}

fn head_name<'a>(base: Base<'_, 'a>) -> Option<&'a str> {
    match base {
        Base::Expression(Expression::Identifier(id)) => Some(id.name.as_str()),
        Base::Expression(Expression::ThisExpression(_)) => Some("this"),
        Base::Expression(Expression::Super(_)) => Some("super"),
        _ => None,
    }
}

fn member_path(doc: &Doc, outer: Member) -> Option<Vec<String>> {
    let (members, base) = spine(outer);
    let head = head_name(base)?;
    let mut path = vec![head.to_owned()];
    path.extend(members.iter().rev().map(|member| member.segment(doc)));
    Some(path)
}

fn spine_reads<S, T: PartialEq<String>>(
    head: Option<&str>,
    segments: &[S],
    path: &[String],
    text: impl Fn(&S) -> T,
) -> bool {
    let Some(first) = segments.len().checked_sub(path.len() - 1) else {
        return false;
    };

    head == Some(path[0].as_str())
        && segments[first..]
            .iter()
            .zip(path.iter().rev())
            .all(|(segment, name)| text(segment) == *name)
}

fn assignment_binds(doc: &Doc, target: &AssignmentTarget) -> Option<Binding> {
    let wrapped = match target {
        AssignmentTarget::TSAsExpression(inner) => Some(&inner.expression),
        AssignmentTarget::TSSatisfiesExpression(inner) => Some(&inner.expression),
        AssignmentTarget::TSNonNullExpression(inner) => Some(&inner.expression),
        AssignmentTarget::TSTypeAssertion(inner) => Some(&inner.expression),
        _ => None,
    };

    let member = match wrapped {
        Some(expression) => match unwrap_path(expression) {
            Unwrapped::Member(member) => Some(member),
            Unwrapped::Base(Base::Expression(Expression::Identifier(id))) => {
                return Some(Binding::Names(vec![id.name.to_string()]));
            }
            Unwrapped::Base(_) => return Some(Binding::Names(Vec::new())),
        },
        None => target
            .as_simple_assignment_target()
            .and_then(SimpleAssignmentTarget::as_member_expression)
            .map(Member::of),
    };

    if let Some(member) = member {
        return member_path(doc, member).map(Binding::Path);
    }

    let mut names = Vec::new();
    target_names(target, &mut names);
    Some(Binding::Names(
        names.into_iter().map(str::to_owned).collect(),
    ))
}

fn binds_of(doc: &Doc, inner: Inner) -> Option<Binding> {
    let names = |declaration| {
        let names = declaration_names(declaration);
        Some(Binding::Names(
            names.into_iter().map(str::to_owned).collect(),
        ))
    };

    match inner {
        Inner::Declaration(Declaration::VariableDeclaration(declaration)) => names(declaration),
        Inner::Statement(Statement::VariableDeclaration(declaration)) => names(declaration),
        Inner::Statement(Statement::ExpressionStatement(stmt)) => match &stmt.expression {
            Expression::AssignmentExpression(assignment) => assignment_binds(doc, &assignment.left),
            _ => None,
        },
        _ => None,
    }
}

fn declaration_of(inner: Inner) -> Option<Declared> {
    let kind = match inner {
        Inner::Declaration(Declaration::VariableDeclaration(declaration))
        | Inner::Statement(Statement::VariableDeclaration(declaration)) => declaration.kind,
        _ => return None,
    };

    Some(Declared {
        keyword: kind.as_str(),
        let_like: kind == VariableDeclarationKind::Let,
    })
}

fn single_line(doc: &Doc, span: Span) -> bool {
    line_at(doc, span.start as usize) == line_at(doc, span.end as usize - 1)
}

fn trim_end(text: &str) -> &str {
    text.trim_end_matches(is_js_whitespace)
}

fn joined_arm(doc: &Doc, start: usize, body: &Statement) -> Option<String> {
    let mut joined = String::new();
    push_header(doc, start, body, &mut joined)?;
    push_body(doc, body, &mut joined)?;
    Some(joined)
}

fn joined_text(doc: &Doc, stmt: &Statement) -> Option<String> {
    let mut joined = String::new();
    push_body(doc, stmt, &mut joined)?;
    Some(joined)
}

fn push_body(doc: &Doc, mut stmt: &Statement, joined: &mut String) -> Option<()> {
    loop {
        let span = stmt.span();
        if single_line(doc, span) {
            joined.push_str(js_trim(source(doc, span.start as usize, span.end as usize)));
            return Some(());
        }

        let body = match stmt {
            Statement::IfStatement(stmt) if stmt.alternate.is_none() => &stmt.consequent,
            Statement::ForStatement(stmt) => &stmt.body,
            Statement::ForInStatement(stmt) => &stmt.body,
            Statement::ForOfStatement(stmt) => &stmt.body,
            Statement::WhileStatement(stmt) => &stmt.body,
            _ => return None,
        };

        push_header(doc, span.start as usize, body, joined)?;
        stmt = body;
    }
}

fn push_header(doc: &Doc, start: usize, body: &Statement, joined: &mut String) -> Option<()> {
    if matches!(body, Statement::BlockStatement(_)) {
        return None;
    }

    let body_start = body.span().start as usize;
    let header = &doc.text[start..body_start];
    let (first, rest) = header.split_once('\n').unwrap_or((header, ""));
    if rest.contains('\n') || !js_trim(rest).is_empty() {
        return None;
    }

    let before = start + trim_end(&doc.text[start..body_start]).len();
    let last = comment_index(doc, before).checked_sub(1);
    if last
        .and_then(|index| doc.comments.get(index))
        .is_some_and(|comment| comment.end == before && comment.start >= start)
    {
        return None;
    }

    joined.push_str(trim_end(first));
    if !matches!(body, Statement::EmptyStatement(_)) {
        joined.push(' ');
    }

    Some(())
}

fn joined_arms(doc: &Doc, stmt: &Statement) -> Option<Vec<String>> {
    let Statement::IfStatement(first) = stmt else {
        return joined_text(doc, stmt).map(|joined| vec![joined]);
    };

    let mut branch: &IfStatement = first;
    let mut arms = Vec::new();
    loop {
        let consequent = &branch.consequent;
        let arm = joined_arm(doc, branch.span.start as usize, consequent)?;
        arms.push(if arms.is_empty() {
            arm
        } else {
            format!("else {arm}")
        });

        let Some(alternate) = &branch.alternate else {
            return Some(arms);
        };

        let (between, after) = (
            consequent.span().end as usize,
            alternate.span().start as usize,
        );
        if doc
            .comments
            .get(comment_index(doc, between))
            .is_some_and(|comment| comment.end <= after)
        {
            return None;
        }

        if let Statement::IfStatement(next) = alternate {
            branch = next;
            continue;
        }

        let keyword = between
            + doc.text[between..]
                .find("else")
                .expect("an else arm has its keyword");
        arms.push(joined_arm(doc, keyword, alternate)?);
        return Some(arms);
    }
}

fn joined_columns(doc: &Doc, node: Node, layout: Layout) -> Option<Joined> {
    let Node::Statement(stmt) = node else {
        return None;
    };

    let span = stmt.span();
    if single_line(doc, span) {
        return None;
    }

    let arms = joined_arms(doc, stmt)?;
    let start = span.start as usize;
    let line_start = doc.line_starts[line_at(doc, start) - 1];
    let prefix = &doc.text[line_start..start];
    let indent = &prefix[..prefix.len() - prefix.trim_start_matches(is_js_whitespace).len()];

    let widest = arms
        .iter()
        .enumerate()
        .map(|(index, arm)| {
            let lead = if index == 0 { prefix } else { indent };
            columns(&format!("{lead}{arm}"), layout.tab)
        })
        .max()
        .unwrap_or(0);

    Some(Joined {
        columns: widest,
        arms: arms.len(),
    })
}

fn statement_operation(doc: &Doc, stmt: &ExpressionStatement) -> Option<String> {
    let (verb, span) = match &stmt.expression {
        Expression::CallExpression(call) => ("call", call.callee.span()),
        Expression::AssignmentExpression(assignment) => ("assign", assignment.left.span()),
        _ => return None,
    };

    Some(format!(
        "{verb}:{}",
        source(doc, span.start as usize, span.end as usize)
    ))
}

fn operation(doc: &Doc, node: Node) -> Option<String> {
    match node {
        Node::Statement(Statement::ExpressionStatement(stmt)) => statement_operation(doc, stmt),
        _ => None,
    }
}

struct PathReads<'d, 'p> {
    doc: &'d Doc<'d>,
    path: &'p [String],
    shadows: bool,
    found: bool,
}

impl PathReads<'_, '_> {
    fn heritage(&mut self, name: &TSTypeName) -> bool {
        if let TSTypeName::QualifiedName(_) = name {
            let mut segments = Vec::new();
            let head = type_name_spine(name, &mut segments);
            self.found |= spine_reads(head, &segments, self.path, |segment| *segment);
        }

        self.found
    }

    fn shadowed(&self, names: &[&str]) -> bool {
        self.shadows && names.contains(&self.path[0].as_str())
    }

    fn member(&mut self, outer: Member) {
        let (members, base) = spine(outer);
        if spine_reads(head_name(base), &members, self.path, |member| {
            member.segment(self.doc)
        }) {
            self.found = true;
            return;
        }

        match base {
            Base::Expression(expression) => self.visit_expression(expression),
            Base::Call(call) => self.visit_call_expression(call),
        }

        for member in members {
            if let Member::Computed(member) = member {
                self.visit_expression(&member.expression);
            }
        }
    }
}

fn type_name_spine<'a>(name: &TSTypeName<'a>, segments: &mut Vec<&'a str>) -> Option<&'a str> {
    match name {
        TSTypeName::IdentifierReference(id) => Some(id.name.as_str()),
        TSTypeName::ThisExpression(_) => Some("this"),
        TSTypeName::QualifiedName(qualified) => {
            segments.push(qualified.right.name.as_str());
            type_name_spine(&qualified.left, segments)
        }
    }
}

impl<'a> Visit<'a> for PathReads<'_, '_> {
    fn visit_statement(&mut self, it: &Statement<'a>) {
        if !self.found {
            walk_statement(self, it);
        }
    }

    fn visit_expression(&mut self, it: &Expression<'a>) {
        if !self.found {
            walk_expression(self, it);
        }
    }

    fn visit_function(&mut self, it: &Function<'a>, flags: ScopeFlags) {
        match it.r#type {
            FunctionType::FunctionExpression | FunctionType::FunctionDeclaration => {}
            _ => walk_function(self, it, flags),
        }
    }

    fn visit_arrow_function_expression(&mut self, _it: &ArrowFunctionExpression<'a>) {}

    fn visit_function_body(&mut self, it: &FunctionBody<'a>) {
        if !self.shadowed(&body_declared(&it.statements)) {
            walk_function_body(self, it);
        }
    }

    fn visit_block_statement(&mut self, it: &BlockStatement<'a>) {
        if !self.shadowed(&body_declared(&it.body)) {
            walk_block_statement(self, it);
        }
    }

    fn visit_static_block(&mut self, it: &StaticBlock<'a>) {
        if !self.shadowed(&body_declared(&it.body)) {
            walk_static_block(self, it);
        }
    }

    fn visit_for_statement(&mut self, it: &ForStatement<'a>) {
        if !self.shadowed(&for_declared(it.init.as_ref())) {
            walk_for_statement(self, it);
        }
    }

    fn visit_for_in_statement(&mut self, it: &ForInStatement<'a>) {
        if !self.shadowed(&left_declared(&it.left)) {
            walk_for_in_statement(self, it);
        }
    }

    fn visit_for_of_statement(&mut self, it: &ForOfStatement<'a>) {
        if !self.shadowed(&left_declared(&it.left)) {
            walk_for_of_statement(self, it);
        }
    }

    fn visit_switch_statement(&mut self, it: &SwitchStatement<'a>) {
        if self.shadowed(&switch_declared(it)) {
            self.visit_expression(&it.discriminant);
        } else {
            walk_switch_statement(self, it);
        }
    }

    fn visit_catch_clause(&mut self, it: &CatchClause<'a>) {
        if !self.shadowed(&catch_declared(it)) {
            oxc_ast_visit::walk::walk_catch_clause(self, it);
        }
    }

    fn visit_jsx_opening_element(&mut self, it: &JSXOpeningElement<'a>) {
        if let Some(element) = jsx_element_path(&it.name)
            && element.len() >= self.path.len()
            && self
                .path
                .iter()
                .zip(&element)
                .all(|(segment, name)| segment == name)
        {
            self.found = true;
            return;
        }

        walk_jsx_opening_element(self, it);
    }

    fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
        self.member(Member::Static(it));
    }

    fn visit_computed_member_expression(&mut self, it: &ComputedMemberExpression<'a>) {
        self.member(Member::Computed(it));
    }

    fn visit_private_field_expression(&mut self, it: &PrivateFieldExpression<'a>) {
        self.member(Member::Private(it));
    }

    fn visit_ts_class_implements(&mut self, it: &TSClassImplements<'a>) {
        if !self.heritage(&it.expression) {
            walk_ts_class_implements(self, it);
        }
    }

    fn visit_ts_interface_heritage(&mut self, it: &TSInterfaceHeritage<'a>) {
        if !self.heritage(&it.type_name) {
            walk_ts_interface_heritage(self, it);
        }
    }
}

fn reads_path(doc: &Doc, inner: Inner, path: &[String]) -> bool {
    let mut walker = PathReads {
        doc,
        path,
        shadows: path[0] != "this" && path[0] != "super",
        found: false,
    };

    match root_function(inner) {
        Some(function) if walker.shadowed(&params_declared(&function.params)) => {}
        Some(function) => walk_function(&mut walker, function, ScopeFlags::Function),
        None => walk_inner(&mut walker, inner),
    }

    walker.found
}

struct Repeats<'d, 'o> {
    doc: &'d Doc<'d>,
    expected: &'o str,
    found: bool,
}

impl<'a> Visit<'a> for Repeats<'_, '_> {
    fn visit_statement(&mut self, it: &Statement<'a>) {
        if !self.found {
            walk_statement(self, it);
        }
    }

    fn visit_function(&mut self, _it: &Function<'a>, _flags: ScopeFlags) {}

    fn visit_arrow_function_expression(&mut self, _it: &ArrowFunctionExpression<'a>) {}

    fn visit_expression_statement(&mut self, it: &ExpressionStatement<'a>) {
        if statement_operation(self.doc, it).as_deref() == Some(self.expected) {
            self.found = true;
        }
    }
}

pub fn facts(doc: &Doc, node: Node, layout: Layout) -> Facts {
    let inner = exported(node);
    let joined = joined_columns(doc, node, layout);
    Facts {
        kind: kind_of(inner),
        word: word_of(inner),
        binds: binds_of(doc, inner),
        declaration: declaration_of(inner),
        guard: guard_of(node),
        compact: single_line(doc, node.span())
            || joined.is_some_and(|joined| joined.columns <= layout.columns),
        joined,
        case_body: matches!(node, Node::Case(case) if !case.consequent.is_empty()),
    }
}

impl Reads for Node<'_> {
    fn references(
        &self,
        _doc: &Doc,
        names: &[&str],
        each: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) {
        references(exported(*self), names, each);
    }

    fn reads_path(&self, doc: &Doc, path: &[String]) -> bool {
        reads_path(doc, exported(*self), path)
    }

    fn operation(&self, doc: &Doc) -> Option<String> {
        operation(doc, *self)
    }

    fn finally_repeats(&self, doc: &Doc, operation: &str) -> bool {
        let Node::Statement(Statement::TryStatement(stmt)) = self else {
            return false;
        };

        stmt.finalizer.as_ref().is_some_and(|finalizer| {
            let mut walker = Repeats {
                doc,
                expected: operation,
                found: false,
            };

            walker.visit_block_statement(finalizer);
            walker.found
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::model::Stmt;
    use crate::engine::tests::{LINEAR_GROWTH_CAP, assert_bounded, cpu_ms, on_deep_stack, ratio};
    use crate::languages::javascript::scan::scanned;

    fn last<R>(path: &str, text: &str, check: impl FnOnce(&Doc, &Stmt<Node>) -> R) -> R {
        scanned(path, text, |doc, scan| {
            let module = scan.lists.first().expect("a module list");
            check(doc, module.stmts.last().expect("a statement"))
        })
    }

    fn referenced(path: &str, text: &str, names: &[&str]) -> Vec<String> {
        last(path, text, |doc, stmt| {
            let mut found = Vec::new();
            stmt.node.references(doc, names, &mut |name| {
                found.push(name.to_owned());
                ControlFlow::Continue(())
            });

            found
        })
    }

    fn reads(file: &str, text: &str, path: &[&str]) -> bool {
        let path: Vec<String> = path.iter().map(|segment| (*segment).to_owned()).collect();
        last(file, text, |doc, stmt| stmt.node.reads_path(doc, &path))
    }

    #[test]
    fn blocks_shadowing_a_read_name_stay_linear_in_their_count() {
        let shadowed = |count: usize| {
            let names: Vec<String> = (1..=count).map(|index| format!("a{index}")).collect();
            let text = format!(
                "if (c) {{\n{}  g({});\n}}\n",
                "  {{ let a1 = 0; }}\n".repeat(count),
                names.join(" + ")
            );

            (names, text)
        };

        on_deep_stack(move || {
            let [(small_names, small), (large_names, large)] = [2_000, 8_000].map(shadowed);
            last("a.ts", &small, |small_doc, small_stmt| {
                last("a.ts", &large, |large_doc, large_stmt| {
                    let walks = [
                        (small_doc, small_stmt, &small_names),
                        (large_doc, large_stmt, &large_names),
                    ];

                    assert_bounded(
                        "shadowing blocks",
                        &[0, 1],
                        5,
                        ratio(LINEAR_GROWTH_CAP),
                        |index| walks[index],
                        |&(doc, stmt, names)| {
                            let names: Vec<&str> = names.iter().map(String::as_str).collect();
                            cpu_ms(|| {
                                stmt.node
                                    .references(doc, &names, &mut |_| ControlFlow::Continue(()));
                            })
                        },
                    );
                });
            });
        });
    }

    #[test]
    fn nested_joined_arms_are_linear() {
        fn statement<'a>(stmt: &Stmt<Node<'a>>) -> &'a Statement<'a> {
            match stmt.node {
                Node::Statement(stmt) => stmt,
                _ => panic!("a statement"),
            }
        }

        let nested = |depth: usize| format!("{}f();\n", "if (x)\n".repeat(depth));

        on_deep_stack(move || {
            let [small, large] = [2_000, 8_000].map(nested);
            last("a.ts", &small, |small_doc, small_stmt| {
                last("a.ts", &large, |large_doc, large_stmt| {
                    let measured = [
                        (small_doc, statement(small_stmt)),
                        (large_doc, statement(large_stmt)),
                    ];

                    assert_bounded(
                        "nested joined arms",
                        &[0, 1],
                        5,
                        ratio(LINEAR_GROWTH_CAP),
                        |index| measured[index],
                        |&(doc, stmt)| {
                            cpu_ms(|| {
                                assert!(joined_arms(doc, stmt).is_some_and(|arms| arms.len() == 1));
                            })
                        },
                    );
                });
            });
        });
    }

    #[test]
    fn closures_are_invisible() {
        let text = "f(() => a, function () { return a; }, a);\n";
        assert_eq!(referenced("a.ts", text, &["a"]), ["a"]);
        assert!(referenced("a.ts", "{ function g() { a; } }\n", &["a"]).is_empty());
        assert!(!reads("a.ts", "f(() => a.b);\n", &["a", "b"]));
        assert!(reads("a.ts", "f(a.b);\n", &["a", "b"]));
    }

    #[test]
    fn root_function_reads_keep_parameter_shadowing() {
        for prefix in ["", "export ", "export default "] {
            let text = format!(
                "{prefix}function f(a = outer) {{ use(a, outer); function g() {{ use(hidden); }} }}\n"
            );
            assert_eq!(
                referenced("a.ts", &text, &["a", "outer", "hidden"]),
                ["outer", "outer"],
                "{text}"
            );

            let text = format!(
                "{prefix}function f(a) {{ use(a.b, outer.b); function g() {{ use(hidden.b); }} }}\n"
            );
            assert!(reads("a.ts", &text, &["outer", "b"]), "{text}");
            assert!(!reads("a.ts", &text, &["a", "b"]), "{text}");
            assert!(!reads("a.ts", &text, &["hidden", "b"]), "{text}");
        }
    }

    #[test]
    fn var_never_shadows() {
        assert_eq!(referenced("a.ts", "{ var a = 1; a; }\n", &["a"]), ["a"]);
        assert!(referenced("a.ts", "{ let a = 1; a; }\n", &["a"]).is_empty());
        assert!(referenced("a.ts", "for (const a of b) a;\n", &["a"]).is_empty());
        assert_eq!(referenced("a.ts", "for (var a of b) a;\n", &["a"]), ["a"]);
    }

    #[test]
    fn a_switch_discriminant_reads_outside_its_case_declarations() {
        let text = "switch (a) { case 1: let a = 2; }\n";
        assert_eq!(referenced("a.ts", text, &["a"]), ["a"]);
        let inner = "switch (b) { case 1: let a = 2; use(a); }\n";
        assert!(referenced("a.ts", inner, &["a"]).is_empty());
        let member = "switch (a.b) { case 1: let a = 2; }\n";
        assert_eq!(referenced("a.ts", member, &["a", "b"]), ["a"]);
        assert!(reads(
            "a.ts",
            "switch (a.b) { case 1: let a = 2; }\n",
            &["a", "b"]
        ));
    }

    #[test]
    fn heritage_names_read_paths() {
        assert!(reads("a.ts", "class C implements a.b {}\n", &["a", "b"]));
        assert!(reads(
            "a.ts",
            "interface I extends X, a.b.c<T> {}\n",
            &["a", "b"]
        ));
        assert!(!reads("a.ts", "interface I extends a {}\n", &["a", "b"]));
        for text in [
            "class C extends a.b implements X.Y {}\n",
            "interface I extends a.b, X.Y {}\n",
            "@a.b class D implements X.Y {}\n",
            "class E implements a.b, X.Y {}\n",
        ] {
            assert!(reads("a.ts", text, &["a", "b"]), "{text}");
        }
    }

    #[test]
    fn member_and_heritage_paths_share_prefix_matching() {
        let bound = ["a", "b"];
        assert!(reads("a.ts", "if (a.b.c) x();\n", &bound));
        assert!(!reads("a.ts", "if (a.c) x();\n", &bound));
        assert!(reads("a.ts", "if (a?.b.c) x();\n", &bound));
        assert!(reads("a.ts", "class C implements a.b.c {}\n", &bound));

        let computed = ["this", "a", "[b]", "c"];
        assert!(reads("a.ts", "if (this.a[b].c.d) x();\n", &computed));
    }

    #[test]
    fn meta_properties_read_nothing() {
        let names = ["import", "meta", "new", "target", "url"];
        assert!(referenced("a.mjs", "f(import.meta.url);\n", &names).is_empty());
        let inside = "function g() { f(new.target); }\n";
        assert!(referenced("a.ts", inside, &names).is_empty());
        let beside = "function g() { f(meta && import.meta, target, new.target); }\n";
        assert_eq!(referenced("a.ts", beside, &names), ["meta", "target"]);
    }

    #[test]
    fn lowercase_and_dashed_jsx_names_do_not_count() {
        let names = ["div", "Foo", "Bar"];
        let text = "f(<div><my-el /><Foo.Bar /><Bar /></div>);\n";
        assert_eq!(referenced("a.tsx", text, &names), ["Foo", "Bar"]);
        assert!(reads("a.tsx", "f(<Foo.Bar.Baz />);\n", &["Foo", "Bar"]));
        assert!(!reads("a.tsx", "f(<foo-bar />);\n", &["foo-bar"]));
    }

    #[test]
    fn non_computed_keys_and_member_properties_are_skipped() {
        let text = "f({ a: 1, [a]: 2 }, b.a, b[a], class { a = 1; [a]() {} });\n";
        assert_eq!(referenced("a.ts", text, &["a"]), ["a", "a", "a"]);
        assert!(referenced("a.ts", "label: for (;;) break label;\n", &["label"]).is_empty());
    }

    #[test]
    fn binds_follow_the_reference_shapes() {
        let binds = |text: &str| last("a.ts", text, |_, stmt| stmt.facts.binds.clone());
        let names = |names: &[&str]| {
            Some(Binding::Names(
                names.iter().map(|name| (*name).to_owned()).collect(),
            ))
        };

        let path = |path: &[&str]| {
            Some(Binding::Path(
                path.iter().map(|segment| (*segment).to_owned()).collect(),
            ))
        };

        assert_eq!(
            binds("let { a, b: [c, ...d], e = 1 } = f;\n"),
            names(&["a", "c", "d", "e"])
        );
        assert_eq!(
            binds("this.a[b].#c = 1;\n"),
            path(&["this", "a", "[b]", "#c"])
        );
        for target in [
            "(a as any)",
            "(a satisfies any)",
            "a!",
            "(<any>a)",
            "((a as any)!)",
        ] {
            assert_eq!(
                binds(&format!("{target} = 1;\n")),
                names(&["a"]),
                "{target}"
            );
        }

        assert_eq!(binds("(a.b as any) = 1;\n"), path(&["a", "b"]));
        assert_eq!(binds("(f().a as any) = 1;\n"), None);
        assert_eq!(binds("f().a = 1;\n"), None);
        assert_eq!(binds("[a, { b }] = c;\n"), names(&["a", "b"]));
    }

    #[test]
    fn a_finally_repeats_an_operation_outside_closures() {
        let repeats = |text: &str, operation: &str| {
            last("a.ts", text, |doc, stmt| {
                stmt.node.finally_repeats(doc, operation)
            })
        };

        assert!(repeats(
            "try { a(); } finally { lock.release(); }\n",
            "call:lock.release"
        ));
        assert!(!repeats(
            "try {} finally { f(() => lock.release()); }\n",
            "call:lock.release"
        ));
        assert!(repeats(
            "try {} finally { if (x) { a = 1; } }\n",
            "assign:a"
        ));
    }
}
