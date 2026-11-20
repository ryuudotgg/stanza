use std::collections::{HashMap, HashSet};

use oxc_allocator::Allocator;
use oxc_ast::AstKind;
use oxc_ast::ast::{
    ArrowFunctionBody, ArrowFunctionExpression, AssignmentTarget, AssignmentTargetMaybeDefault,
    AssignmentTargetProperty, AssignmentTargetPropertyIdentifier, BindingIdentifier,
    BindingPattern, CallExpression, ChainElement, Class, ConditionalExpression, Declaration,
    ExportDefaultDeclarationKind, Expression, Function, IdentifierReference, ImportDeclaration,
    ImportOrExportKind, ImportSpecifier, LogicalExpression, LogicalOperator, MemberExpression,
    ModuleExportName, Program, PropertyKey, SimpleAssignmentTarget, Statement, TSEnumDeclaration,
    TSExternalModuleDeclaration, TSGlobalDeclaration, TSImportEqualsDeclaration,
    TSInterfaceDeclaration, TSNamespaceDeclaration, TSType, TSTypeAliasDeclaration,
    TSTypeParameterDeclaration, VariableDeclaration, VariableDeclarationKind,
};
use oxc_ast_visit::{Visit, walk};
use oxc_syntax::scope::ScopeFlags;

use crate::languages::javascript::ast::{body_declared, params_declared, pattern_names};
use crate::languages::javascript::nesting::{crossing, thread_cap};
use crate::languages::javascript::parse::{parse as parse_program, source_type};

pub fn parse(path: &str, text: &str) -> Option<&'static Program<'static>> {
    if crossing(text, source_type(path), thread_cap()).is_some() {
        return None;
    }

    // A config module lives for the whole process, so its arena is leaked and its nodes are 'static; nodes are compared by address.
    let allocator: &'static Allocator = Box::leak(Box::default());
    let source = allocator.alloc_str(text);
    let parsed = parse_program(allocator, path, source);
    parsed
        .diagnostics
        .is_empty()
        .then(|| &*allocator.alloc(parsed.program))
}

struct EachNode<'f>(&'f mut dyn FnMut(AstKind<'static>));

impl Visit<'static> for EachNode<'_> {
    fn enter_node(&mut self, kind: AstKind<'static>) {
        (self.0)(kind);
    }
}

pub fn each_node(program: &'static Program<'static>, mut enter: impl FnMut(AstKind<'static>)) {
    EachNode(&mut enter).visit_program(program);
}

pub fn plain_value(expression: &Expression<'_>) -> bool {
    match expression {
        Expression::StringLiteral(_)
        | Expression::NumericLiteral(_)
        | Expression::BigIntLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_) => true,
        Expression::TemplateLiteral(template) => template.expressions.is_empty(),
        _ => false,
    }
}

pub fn runs_unseen_code(kind: AstKind<'_>) -> bool {
    let source = match kind {
        AstKind::ImportDeclaration(import) if import.import_kind != ImportOrExportKind::Type => {
            import.source.value.as_str()
        }
        AstKind::ExportAllDeclaration(export) if export.export_kind != ImportOrExportKind::Type => {
            export.source.value.as_str()
        }
        AstKind::ExportFromDeclaration(export)
            if export.export_kind != ImportOrExportKind::Type =>
        {
            export.source.value.as_str()
        }
        AstKind::ImportExpression(import) => match &import.source {
            Expression::StringLiteral(literal) => literal.value.as_str(),
            _ => return true,
        },
        AstKind::CallExpression(call) => {
            let callee = call.callee.get_inner_expression();
            let require = identifier_name(callee)
                .or_else(|| callee.as_member_expression().and_then(member_name));
            if require != Some("require") {
                return false;
            }

            match call
                .arguments
                .first()
                .and_then(|argument| argument.as_expression())
            {
                Some(Expression::StringLiteral(literal)) => literal.value.as_str(),
                _ => return true,
            }
        }
        AstKind::IdentifierReference(reference) => {
            return matches!(reference.name.as_str(), "eval" | "Function");
        }
        AstKind::StaticMemberExpression(member) => {
            return code_property(member.property.name.as_str());
        }
        AstKind::ComputedMemberExpression(member) => {
            return match &member.expression {
                Expression::StringLiteral(literal) => code_property(literal.value.as_str()),
                Expression::NumericLiteral(_) => false,
                _ => true,
            };
        }
        _ => return false,
    };

    let name = source.strip_prefix("node:").unwrap_or(source);
    matches!(name, "module" | "vm")
        || name.contains(':')
        || !name.starts_with(|first: char| first == '@' || first.is_ascii_alphanumeric())
}

fn code_property(name: &str) -> bool {
    matches!(name, "constructor" | "getBuiltinModule")
}

pub fn global_root<'a>(target: &AssignmentTarget<'a>) -> Option<&'a str> {
    let member = target.as_member_expression()?;
    member_name(member)?;
    match member.object() {
        Expression::StaticMemberExpression(env) if env.property.name == "env" => {
            identifier_name(&env.object).filter(|root| *root == "process")
        }
        object => identifier_name(object).filter(|root| ["globalThis", "global"].contains(root)),
    }
}

pub fn key_name<'a>(key: &PropertyKey<'a>) -> Option<&'a str> {
    match key {
        PropertyKey::StaticIdentifier(name) => Some(name.name.as_str()),
        PropertyKey::StringLiteral(literal) => Some(literal.value.as_str()),
        _ => None,
    }
}

pub fn export_name<'a>(name: &ModuleExportName<'a>) -> &'a str {
    match name {
        ModuleExportName::IdentifierName(name) => name.name.as_str(),
        ModuleExportName::IdentifierReference(reference) => reference.name.as_str(),
        ModuleExportName::StringLiteral(literal) => literal.value.as_str(),
    }
}

pub fn identifier_name<'a>(expression: &Expression<'a>) -> Option<&'a str> {
    match expression {
        Expression::Identifier(reference) => Some(reference.name.as_str()),
        _ => None,
    }
}

pub fn member_name<'a>(member: &MemberExpression<'a>) -> Option<&'a str> {
    match member {
        MemberExpression::StaticMemberExpression(member) => Some(member.property.name.as_str()),
        MemberExpression::ComputedMemberExpression(member) => match &member.expression {
            Expression::StringLiteral(literal) => Some(literal.value.as_str()),
            _ => None,
        },
        MemberExpression::PrivateFieldExpression(_) => None,
    }
}

pub fn member_of<'b, 'a>(expression: &'b Expression<'a>) -> Option<&'b MemberExpression<'a>> {
    match expression.get_inner_expression() {
        Expression::ChainExpression(chain) => match &chain.expression {
            ChainElement::TSNonNullExpression(inner) => member_of(&inner.expression),
            element => element.as_member_expression(),
        },
        inner => inner.as_member_expression(),
    }
}

pub fn root_name<'a>(mut expression: &Expression<'a>) -> Option<&'a str> {
    while let Some(member) = member_of(expression) {
        expression = member.object();
    }

    identifier_name(expression.get_inner_expression())
}

pub fn root_names<'a>(expression: &Expression<'a>) -> Vec<&'a str> {
    let mut names = Vec::new();
    let mut pending = vec![expression];
    while let Some(mut node) = pending.pop() {
        loop {
            if let Expression::LogicalExpression(logical) = node.get_inner_expression() {
                pending.push(&logical.right);
                pending.push(&logical.left);
                break;
            }

            let Some(member) = member_of(node) else {
                names.extend(identifier_name(node.get_inner_expression()));
                break;
            };

            node = member.object();
        }
    }

    names
}

pub fn target_write<'a>(target: &SimpleAssignmentTarget<'a>) -> (Option<&'a str>, bool) {
    if let SimpleAssignmentTarget::AssignmentTargetIdentifier(reference) = target {
        return (Some(reference.name.as_str()), false);
    }

    match target
        .get_expression()
        .map(Expression::get_inner_expression)
    {
        Some(inner) => (root_name(inner), inner.is_member_expression()),
        None => (
            target
                .as_member_expression()
                .and_then(|member| root_name(member.object())),
            true,
        ),
    }
}

pub fn target_writes<'a>(
    target: &AssignmentTarget<'a>,
    write: &mut impl FnMut(Option<&'a str>, bool),
) {
    if let Some(simple) = target.as_simple_assignment_target() {
        let (root, member) = target_write(simple);
        return write(root, member);
    }

    let rest = match target {
        AssignmentTarget::ArrayAssignmentTarget(array) => {
            for element in array.elements.iter().flatten() {
                default_writes(element, write);
            }

            &array.rest
        }
        AssignmentTarget::ObjectAssignmentTarget(object) => {
            for property in &object.properties {
                match property {
                    AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(it) => {
                        write(Some(it.binding.name.as_str()), false);
                    }
                    AssignmentTargetProperty::AssignmentTargetPropertyProperty(it) => {
                        default_writes(&it.binding, write);
                    }
                }
            }

            &object.rest
        }
        _ => return,
    };

    if let Some(rest) = rest {
        target_writes(&rest.target, write);
    }
}

fn default_writes<'a>(
    target: &AssignmentTargetMaybeDefault<'a>,
    write: &mut impl FnMut(Option<&'a str>, bool),
) {
    match target {
        AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(it) => {
            target_writes(&it.binding, write);
        }
        _ => {
            if let Some(target) = target.as_assignment_target() {
                target_writes(target, write);
            }
        }
    }
}

#[derive(Default)]
struct RequireBound(bool);

impl Visit<'static> for RequireBound {
    fn visit_binding_identifier(&mut self, it: &BindingIdentifier<'static>) {
        self.0 |= it.name == "require";
    }

    fn visit_simple_assignment_target(&mut self, it: &SimpleAssignmentTarget<'static>) {
        self.0 |= target_write(it) == (Some("require"), false);
        walk::walk_simple_assignment_target(self, it);
    }

    fn visit_assignment_target_property_identifier(
        &mut self,
        it: &AssignmentTargetPropertyIdentifier<'static>,
    ) {
        self.0 |= it.binding.name == "require";
        walk::walk_assignment_target_property_identifier(self, it);
    }

    fn visit_ts_type(&mut self, _: &TSType<'static>) {}

    fn visit_ts_type_alias_declaration(&mut self, _: &TSTypeAliasDeclaration<'static>) {}

    fn visit_ts_interface_declaration(&mut self, _: &TSInterfaceDeclaration<'static>) {}

    fn visit_ts_type_parameter_declaration(&mut self, _: &TSTypeParameterDeclaration<'static>) {}

    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'static>) {
        if !it.import_kind.is_type() {
            walk::walk_import_declaration(self, it);
        }
    }

    fn visit_import_specifier(&mut self, it: &ImportSpecifier<'static>) {
        if !it.import_kind.is_type() {
            walk::walk_import_specifier(self, it);
        }
    }

    fn visit_ts_import_equals_declaration(&mut self, it: &TSImportEqualsDeclaration<'static>) {
        if !it.import_kind.is_type() {
            walk::walk_ts_import_equals_declaration(self, it);
        }
    }

    fn visit_variable_declaration(&mut self, it: &VariableDeclaration<'static>) {
        if !it.declare {
            walk::walk_variable_declaration(self, it);
        }
    }

    fn visit_function(&mut self, it: &Function<'static>, flags: ScopeFlags) {
        if !it.declare && it.body.is_some() {
            walk::walk_function(self, it, flags);
        }
    }

    fn visit_class(&mut self, it: &Class<'static>) {
        if !it.declare {
            walk::walk_class(self, it);
        }
    }

    fn visit_ts_enum_declaration(&mut self, it: &TSEnumDeclaration<'static>) {
        if !it.declare {
            walk::walk_ts_enum_declaration(self, it);
        }
    }

    fn visit_ts_namespace_declaration(&mut self, it: &TSNamespaceDeclaration<'static>) {
        if !it.declare {
            walk::walk_ts_namespace_declaration(self, it);
        }
    }

    fn visit_ts_external_module_declaration(&mut self, _: &TSExternalModuleDeclaration<'static>) {}

    fn visit_ts_global_declaration(&mut self, _: &TSGlobalDeclaration<'static>) {}
}

pub fn binds_require(program: &'static Program<'static>) -> bool {
    let mut bound = RequireBound::default();
    bound.visit_program(program);
    bound.0
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Known {
    Nullish,
    Falsy,
    Truthy,
}

const KNOWN_DEPTH: usize = 8;

struct EagerRead<'p> {
    pending: &'p HashSet<&'static str>,
    constants: &'p HashMap<&'static str, &'static Expression<'static>>,
    hidden: HashMap<&'static str, usize>,
    found: bool,
}

impl EagerRead<'_> {
    fn hiding(&mut self, names: Vec<&'static str>, walk: impl FnOnce(&mut Self)) {
        for name in &names {
            *self.hidden.entry(name).or_default() += 1;
        }

        walk(self);

        for name in names {
            if let Some(count) = self.hidden.get_mut(name) {
                *count -= 1;
                if *count == 0 {
                    self.hidden.remove(name);
                }
            }
        }
    }

    fn known(&self, expression: &Expression<'static>, depth: usize) -> Option<Known> {
        let truthy = |yes: bool| if yes { Known::Truthy } else { Known::Falsy };
        match expression.get_inner_expression() {
            Expression::NullLiteral(_) => Some(Known::Nullish),
            Expression::BooleanLiteral(literal) => Some(truthy(literal.value)),
            Expression::NumericLiteral(literal) => {
                Some(truthy(literal.value != 0.0 && !literal.value.is_nan()))
            }
            Expression::StringLiteral(literal) => Some(truthy(!literal.value.is_empty())),
            Expression::ObjectExpression(_)
            | Expression::ArrayExpression(_)
            | Expression::FunctionExpression(_)
            | Expression::ArrowFunctionExpression(_)
            | Expression::ClassExpression(_)
            | Expression::RegExpLiteral(_) => Some(Known::Truthy),
            Expression::Identifier(reference)
                if depth < KNOWN_DEPTH
                    && !self.pending.contains(reference.name.as_str())
                    && !self.hidden.contains_key(reference.name.as_str()) =>
            {
                let init = self.constants.get(reference.name.as_str())?;
                self.known(init, depth + 1)
            }
            _ => None,
        }
    }
}

impl Visit<'static> for EagerRead<'_> {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'static>) {
        let name = it.name.as_str();
        self.found |= self.pending.contains(name) && !self.hidden.contains_key(name);
    }

    fn visit_logical_expression(&mut self, it: &LogicalExpression<'static>) {
        self.visit_expression(&it.left);
        let left = self.known(&it.left, 0);
        let skipped = match it.operator {
            LogicalOperator::Or => left == Some(Known::Truthy),
            LogicalOperator::And => left.is_some_and(|known| known != Known::Truthy),
            LogicalOperator::Coalesce => left.is_some_and(|known| known != Known::Nullish),
        };

        if !skipped {
            self.visit_expression(&it.right);
        }
    }

    fn visit_conditional_expression(&mut self, it: &ConditionalExpression<'static>) {
        self.visit_expression(&it.test);
        match self.known(&it.test, 0) {
            Some(Known::Truthy) => self.visit_expression(&it.consequent),
            Some(_) => self.visit_expression(&it.alternate),
            None => {
                self.visit_expression(&it.consequent);
                self.visit_expression(&it.alternate);
            }
        }
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'static>) {
        match it.callee.get_inner_expression() {
            Expression::ArrowFunctionExpression(arrow) => {
                let mut local = params_declared(&arrow.params);
                if let ArrowFunctionBody::FunctionBody(body) = &arrow.body {
                    local.extend(body_declared(&body.statements));
                }

                self.hiding(local, |read| read.visit_arrow_function_body(&arrow.body));
            }
            Expression::FunctionExpression(function) if !function.generator => {
                if let Some(body) = &function.body {
                    let mut local = params_declared(&function.params);
                    local.extend(body_declared(&body.statements));
                    local.extend(function.id.as_ref().map(|id| id.name.as_str()));
                    self.hiding(local, |read| read.visit_function_body(body));
                }
            }
            _ => {}
        }

        walk::walk_call_expression(self, it);
    }

    fn visit_function(&mut self, _: &Function<'static>, _: ScopeFlags) {}

    fn visit_arrow_function_expression(&mut self, _: &ArrowFunctionExpression<'static>) {}

    fn visit_class(&mut self, _: &Class<'static>) {}

    fn visit_ts_type(&mut self, _: &TSType<'static>) {}
}

fn reads_pending(
    expression: &'static Expression<'static>,
    pending: &HashSet<&'static str>,
    constants: &HashMap<&'static str, &'static Expression<'static>>,
) -> bool {
    let mut read = EagerRead {
        pending,
        constants,
        hidden: HashMap::new(),
        found: false,
    };

    read.visit_expression(expression);
    read.found
}

fn top_declaration(
    statement: &'static Statement<'static>,
) -> Option<&'static Declaration<'static>> {
    match statement {
        Statement::ExportDeclaration(export) => Some(&export.declaration),
        _ => statement.as_declaration(),
    }
}

fn lexical_names(statement: &'static Statement<'static>) -> Vec<&'static str> {
    let mut names = Vec::new();
    let class = match (statement, top_declaration(statement)) {
        (_, Some(Declaration::VariableDeclaration(variables)))
            if variables.kind != VariableDeclarationKind::Var && !variables.declare =>
        {
            for declarator in &variables.declarations {
                pattern_names(&declarator.id, &mut names);
            }

            None
        }
        (_, Some(Declaration::ClassDeclaration(class))) => Some(class),
        (Statement::ExportDefaultDeclaration(export), _) => match &export.declaration {
            ExportDefaultDeclarationKind::ClassDeclaration(class) => Some(class),
            _ => None,
        },
        _ => None,
    };

    let id = class
        .filter(|class| !class.declare)
        .and_then(|class| class.id.as_ref());
    names.extend(id.map(|id| id.name.as_str()));
    names
}

pub fn reads_uninitialized(program: &'static Program<'static>) -> bool {
    let mut pending: HashSet<&'static str> = program.body.iter().flat_map(lexical_names).collect();
    let constants: HashMap<&'static str, &'static Expression<'static>> = program
        .body
        .iter()
        .filter_map(top_declaration)
        .filter_map(|declaration| match declaration {
            Declaration::VariableDeclaration(variables)
                if variables.kind == VariableDeclarationKind::Const =>
            {
                Some(variables)
            }
            _ => None,
        })
        .flat_map(|variables| &variables.declarations)
        .filter_map(|declarator| match (&declarator.id, &declarator.init) {
            (BindingPattern::BindingIdentifier(id), Some(init)) => Some((id.name.as_str(), init)),
            _ => None,
        })
        .collect();

    for statement in &program.body {
        if let Statement::ExportDefaultDeclaration(export) = statement
            && let Some(expression) = export.declaration.as_expression()
            && reads_pending(expression, &pending, &constants)
        {
            return true;
        }

        if let Some(Declaration::VariableDeclaration(variables)) = top_declaration(statement) {
            for declarator in &variables.declarations {
                if declarator
                    .init
                    .as_ref()
                    .is_some_and(|init| reads_pending(init, &pending, &constants))
                {
                    return true;
                }

                let mut names = Vec::new();
                pattern_names(&declarator.id, &mut names);
                for name in names {
                    pending.remove(name);
                }
            }
        }

        for name in lexical_names(statement) {
            pending.remove(name);
        }
    }

    false
}

pub fn imports_files(program: &'static Program<'static>, reaches: impl Fn(&str) -> bool) -> bool {
    let mut imports = false;
    each_node(program, |kind| match kind {
        AstKind::ImportDeclaration(import) => imports |= reaches(import.source.value.as_str()),
        AstKind::CallExpression(call) => imports |= required_source(call).is_some_and(&reaches),
        _ => {}
    });

    imports
}

pub fn required_source<'a>(call: &CallExpression<'a>) -> Option<&'a str> {
    if identifier_name(&call.callee) != Some("require") {
        return None;
    }

    match call.arguments.first()?.as_expression()? {
        Expression::StringLiteral(literal) => Some(literal.value.as_str()),
        _ => None,
    }
}

pub enum Operand {
    Expression(&'static Expression<'static>),
    Member(&'static MemberExpression<'static>),
    Reference(&'static str),
    Opaque,
}

pub struct Property {
    pub start: u32,
    pub key: Option<&'static str>,
    pub value: Operand,
}

fn property_key(key: &PropertyKey<'static>) -> Option<&'static str> {
    match key {
        PropertyKey::Identifier(reference) => Some(reference.name.as_str()),
        _ => key_name(key),
    }
}

pub fn property(kind: AstKind<'static>) -> Option<Property> {
    let (start, key, value) = match kind {
        AstKind::ObjectProperty(it) => (
            it.span.start,
            property_key(&it.key),
            Operand::Expression(&it.value),
        ),
        AstKind::BindingProperty(it) => (
            it.span.start,
            property_key(&it.key),
            match &it.value {
                BindingPattern::BindingIdentifier(id) => Operand::Reference(id.name.as_str()),
                _ => Operand::Opaque,
            },
        ),
        AstKind::AssignmentTargetPropertyIdentifier(it) => (
            it.span.start,
            Some(it.binding.name.as_str()),
            match it.init {
                None => Operand::Reference(it.binding.name.as_str()),
                Some(_) => Operand::Opaque,
            },
        ),
        AstKind::AssignmentTargetPropertyProperty(it) => (
            it.span.start,
            property_key(&it.name),
            target_operand(&it.binding),
        ),
        _ => return None,
    };

    Some(Property { start, key, value })
}

fn target_operand(target: &'static AssignmentTargetMaybeDefault<'static>) -> Operand {
    let Some(target) = target
        .as_assignment_target()
        .and_then(AssignmentTarget::as_simple_assignment_target)
    else {
        return Operand::Opaque;
    };

    if let SimpleAssignmentTarget::AssignmentTargetIdentifier(reference) = target {
        return Operand::Reference(reference.name.as_str());
    }

    target
        .get_expression()
        .map(Operand::Expression)
        .or_else(|| target.as_member_expression().map(Operand::Member))
        .unwrap_or(Operand::Opaque)
}
