use oxc_allocator::Allocator;
use oxc_ast::AstKind;
use oxc_ast::ast::{
    AssignmentTarget, AssignmentTargetMaybeDefault, AssignmentTargetProperty,
    AssignmentTargetPropertyIdentifier, BindingIdentifier, BindingPattern, CallExpression, Class,
    Expression, Function, ImportDeclaration, ImportSpecifier, MemberExpression, ModuleExportName,
    Program, PropertyKey, SimpleAssignmentTarget, TSEnumDeclaration, TSExternalModuleDeclaration,
    TSGlobalDeclaration, TSImportEqualsDeclaration, TSInterfaceDeclaration, TSNamespaceDeclaration,
    TSType, TSTypeAliasDeclaration, TSTypeParameterDeclaration, VariableDeclaration,
};
use oxc_ast_visit::{Visit, walk};
use oxc_syntax::scope::ScopeFlags;

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
