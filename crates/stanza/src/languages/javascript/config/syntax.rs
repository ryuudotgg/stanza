use oxc_allocator::Allocator;
use oxc_ast::AstKind;
use oxc_ast::ast::{
    AssignmentTarget, AssignmentTargetMaybeDefault, BindingPattern, CallExpression, Expression,
    MemberExpression, ModuleExportName, Program, PropertyKey, SimpleAssignmentTarget,
};
use oxc_ast_visit::Visit;

use crate::languages::javascript::parse::parse as parse_program;

pub fn parse(path: &str, text: &str) -> Option<&'static Program<'static>> {
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

pub fn root_name<'a>(expression: &Expression<'a>) -> Option<&'a str> {
    match expression.as_member_expression() {
        Some(member) => root_name(member.object()),
        None => identifier_name(expression),
    }
}

pub fn target_root<'a>(target: &SimpleAssignmentTarget<'a>) -> Option<&'a str> {
    match target {
        SimpleAssignmentTarget::AssignmentTargetIdentifier(reference) => {
            Some(reference.name.as_str())
        }
        _ => root_name(target.as_member_expression()?.object()),
    }
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
