use std::ops::ControlFlow;

use oxc_ast::ast::{
    AccessorProperty, AccessorPropertyType, ArrowFunctionExpression, AssignmentPattern,
    AssignmentTarget, AssignmentTargetMaybeDefault, AssignmentTargetProperty, BindingIdentifier,
    BindingPattern, BindingProperty, BlockStatement, CatchClause, Class, Declaration, Directive,
    Expression, ForInStatement, ForOfStatement, ForStatement, ForStatementInit, ForStatementLeft,
    FormalParameters, Function, FunctionBody, FunctionType, IdentifierName, IdentifierReference,
    JSXClosingElement, JSXElementName, JSXMemberExpressionObject, JSXOpeningElement,
    LabelIdentifier, MethodDefinition, MethodDefinitionType, ModuleExportName, PropertyDefinition,
    PropertyDefinitionType, Statement, StaticBlock, SwitchCase, SwitchStatement, TSEnumDeclaration,
    TSExternalModuleDeclaration, TSGlobalDeclaration, TSImportEqualsDeclaration, TSIndexSignature,
    TSInterfaceDeclaration, TSNamespaceDeclaration, TSNamespaceExportDeclaration, TSThisParameter,
    TSType, TSTypeAliasDeclaration, TSTypeAnnotation, TSTypeParameterDeclaration,
    TSTypeParameterInstantiation, VariableDeclaration, VariableDeclarationKind, VariableDeclarator,
};
use oxc_ast_visit::Visit;
use oxc_ast_visit::walk::{
    walk_block_statement, walk_class_body, walk_expression, walk_for_in_statement,
    walk_for_of_statement, walk_for_statement, walk_function_body, walk_statement,
    walk_static_block, walk_switch_case, walk_switch_statement,
};
use oxc_syntax::scope::ScopeFlags;

#[derive(Clone, Copy, Debug)]
pub enum Inner<'a> {
    Directive(&'a Directive<'a>),
    Statement(&'a Statement<'a>),
    Declaration(&'a Declaration<'a>),
    Function(&'a Function<'a>),
    Case(&'a SwitchCase<'a>),
}

fn add<'a>(names: &mut Vec<&'a str>, name: &'a str) {
    if !names.contains(&name) {
        names.push(name);
    }
}

pub fn pattern_names<'a>(pattern: &BindingPattern<'a>, names: &mut Vec<&'a str>) {
    match pattern {
        BindingPattern::BindingIdentifier(id) => add(names, id.name.as_str()),
        BindingPattern::ObjectPattern(object) => {
            for property in &object.properties {
                pattern_names(&property.value, names);
            }

            if let Some(rest) = &object.rest {
                pattern_names(&rest.argument, names);
            }
        }
        BindingPattern::ArrayPattern(array) => {
            for element in array.elements.iter().flatten() {
                pattern_names(element, names);
            }

            if let Some(rest) = &array.rest {
                pattern_names(&rest.argument, names);
            }
        }
        BindingPattern::AssignmentPattern(assignment) => pattern_names(&assignment.left, names),
    }
}

pub fn declaration_names<'a>(declaration: &VariableDeclaration<'a>) -> Vec<&'a str> {
    let mut names = Vec::new();
    for declarator in &declaration.declarations {
        pattern_names(&declarator.id, &mut names);
    }

    names
}

fn maybe_default_names<'a>(target: &AssignmentTargetMaybeDefault<'a>, names: &mut Vec<&'a str>) {
    match target {
        AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(with) => {
            target_names(&with.binding, names);
        }
        _ => target_names(target.to_assignment_target(), names),
    }
}

pub fn target_names<'a>(target: &AssignmentTarget<'a>, names: &mut Vec<&'a str>) {
    match target {
        AssignmentTarget::AssignmentTargetIdentifier(id) => add(names, id.name.as_str()),
        AssignmentTarget::ArrayAssignmentTarget(array) => {
            for element in array.elements.iter().flatten() {
                maybe_default_names(element, names);
            }

            if let Some(rest) = &array.rest {
                target_names(&rest.target, names);
            }
        }
        AssignmentTarget::ObjectAssignmentTarget(object) => {
            for property in &object.properties {
                match property {
                    AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(shorthand) => {
                        add(names, shorthand.binding.name.as_str());
                    }
                    AssignmentTargetProperty::AssignmentTargetPropertyProperty(property) => {
                        maybe_default_names(&property.binding, names);
                    }
                }
            }

            if let Some(rest) = &object.rest {
                target_names(&rest.target, names);
            }
        }
        _ => {}
    }
}

fn lexical_names<'a>(stmt: &Statement<'a>) -> Vec<&'a str> {
    match stmt {
        Statement::VariableDeclaration(declaration)
            if declaration.kind != VariableDeclarationKind::Var =>
        {
            declaration_names(declaration)
        }
        Statement::FunctionDeclaration(function)
            if function.r#type == FunctionType::FunctionDeclaration =>
        {
            function.id.iter().map(|id| id.name.as_str()).collect()
        }
        Statement::ClassDeclaration(class) => class.id.iter().map(|id| id.name.as_str()).collect(),
        _ => Vec::new(),
    }
}

fn declaration_lexical<'a>(declaration: &VariableDeclaration<'a>) -> Vec<&'a str> {
    if declaration.kind == VariableDeclarationKind::Var {
        Vec::new()
    } else {
        declaration_names(declaration)
    }
}

pub fn body_declared<'a>(body: &[Statement<'a>]) -> Vec<&'a str> {
    body.iter().flat_map(lexical_names).collect()
}

pub fn for_declared<'a>(init: Option<&ForStatementInit<'a>>) -> Vec<&'a str> {
    match init {
        Some(ForStatementInit::VariableDeclaration(declaration)) => {
            declaration_lexical(declaration)
        }
        _ => Vec::new(),
    }
}

pub fn left_declared<'a>(left: &ForStatementLeft<'a>) -> Vec<&'a str> {
    match left {
        ForStatementLeft::VariableDeclaration(declaration) => declaration_lexical(declaration),
        _ => Vec::new(),
    }
}

pub fn switch_declared<'a>(switch: &SwitchStatement<'a>) -> Vec<&'a str> {
    switch
        .cases
        .iter()
        .flat_map(|case| body_declared(&case.consequent))
        .collect()
}

pub fn catch_declared<'a>(clause: &CatchClause<'a>) -> Vec<&'a str> {
    let mut names = Vec::new();
    if let Some(param) = &clause.param {
        pattern_names(&param.pattern, &mut names);
    }

    names
}

pub fn params_declared<'a>(function: &Function<'a>) -> Vec<&'a str> {
    let mut names = Vec::new();
    if function.this_param.is_some() {
        add(&mut names, "this");
    }

    for param in &function.params.items {
        if !param.has_modifier() {
            pattern_names(&param.pattern, &mut names);
        }
    }

    if let Some(rest) = &function.params.rest {
        pattern_names(&rest.rest.argument, &mut names);
    }

    names
}

fn jsx_object_path<'a>(object: &JSXMemberExpressionObject<'a>, segments: &mut Vec<&'a str>) {
    match object {
        JSXMemberExpressionObject::IdentifierReference(id) => segments.push(id.name.as_str()),
        JSXMemberExpressionObject::ThisExpression(_) => segments.push("this"),
        JSXMemberExpressionObject::MemberExpression(member) => {
            jsx_object_path(&member.object, segments);
            segments.push(member.property.name.as_str());
        }
    }
}

pub fn jsx_element_path<'a>(name: &JSXElementName<'a>) -> Option<Vec<&'a str>> {
    let single = |name: &'a str| {
        let lower = name.starts_with(|char: char| char.is_ascii_lowercase());
        (!lower && !name.contains('-')).then(|| vec![name])
    };

    match name {
        JSXElementName::Identifier(id) => single(id.name.as_str()),
        JSXElementName::IdentifierReference(id) => single(id.name.as_str()),
        JSXElementName::ThisExpression(_) => single("this"),
        JSXElementName::NamespacedName(_) => None,
        JSXElementName::MemberExpression(member) => {
            let mut segments = Vec::new();
            jsx_object_path(&member.object, &mut segments);
            segments.push(member.property.name.as_str());
            Some(segments)
        }
    }
}

pub fn root_address(inner: Inner) -> usize {
    match inner {
        Inner::Statement(Statement::FunctionDeclaration(function)) => {
            std::ptr::from_ref::<Function>(function).addr()
        }
        Inner::Declaration(Declaration::FunctionDeclaration(function)) => {
            std::ptr::from_ref::<Function>(function).addr()
        }
        Inner::Function(function) => std::ptr::from_ref(function).addr(),
        _ => 0,
    }
}

pub fn is_root(root: usize, function: &Function) -> bool {
    std::ptr::from_ref(function).addr() == root
}

pub fn walk_inner<'a, V: Visit<'a>>(visitor: &mut V, inner: Inner<'a>) {
    match inner {
        Inner::Directive(directive) => visitor.visit_directive(directive),
        Inner::Statement(stmt) => visitor.visit_statement(stmt),
        Inner::Declaration(declaration) => visitor.visit_declaration(declaration),
        Inner::Function(function) => visitor.visit_function(function, ScopeFlags::empty()),
        Inner::Case(case) => visitor.visit_switch_case(case),
    }
}

struct References<'n, 'e> {
    visible: Vec<&'n str>,
    binding: bool,
    root: usize,
    each: &'e mut dyn FnMut(&str) -> ControlFlow<()>,
    stopped: bool,
}

impl References<'_, '_> {
    fn emit(&mut self, name: &str) {
        if self.stopped || self.binding || !self.visible.contains(&name) {
            return;
        }

        if (self.each)(name).is_break() {
            self.stopped = true;
        }
    }

    fn scoped(&mut self, shadowed: &[&str], walk: impl FnOnce(&mut Self)) {
        if self.stopped {
            return;
        }

        if !shadowed.iter().any(|name| self.visible.contains(name)) {
            walk(self);
            return;
        }

        let visible: Vec<_> = self
            .visible
            .iter()
            .copied()
            .filter(|name| !shadowed.contains(name))
            .collect();

        if visible.is_empty() {
            return;
        }

        let outer = std::mem::replace(&mut self.visible, visible);
        walk(self);
        self.visible = outer;
    }

    fn with_binding(&mut self, binding: bool, walk: impl FnOnce(&mut Self)) {
        let outer = std::mem::replace(&mut self.binding, binding);
        walk(self);
        self.binding = outer;
    }

    fn params<'a>(&mut self, params: &FormalParameters<'a>) {
        for param in &params.items {
            if param.has_modifier() {
                continue;
            }

            self.with_binding(true, |walker| walker.visit_binding_pattern(&param.pattern));
            if let Some(initializer) = &param.initializer {
                self.with_binding(false, |walker| walker.visit_expression(initializer));
            }
        }

        if let Some(rest) = &params.rest {
            self.with_binding(true, |walker| {
                walker.visit_binding_pattern(&rest.rest.argument);
            });
        }
    }
}

impl<'a> Visit<'a> for References<'_, '_> {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        self.emit(it.name.as_str());
    }

    fn visit_binding_identifier(&mut self, it: &BindingIdentifier<'a>) {
        self.emit(it.name.as_str());
    }

    fn visit_identifier_name(&mut self, _it: &IdentifierName<'a>) {}

    fn visit_label_identifier(&mut self, _it: &LabelIdentifier<'a>) {}

    fn visit_module_export_name(&mut self, it: &ModuleExportName<'a>) {
        match it {
            ModuleExportName::IdentifierName(name) => self.emit(name.name.as_str()),
            ModuleExportName::IdentifierReference(name) => self.emit(name.name.as_str()),
            ModuleExportName::StringLiteral(_) => {}
        }
    }

    fn visit_expression(&mut self, it: &Expression<'a>) {
        if self.stopped {
            return;
        }

        match it {
            Expression::ImportMeta(_) => {
                self.emit("import");
                self.emit("meta");
            }
            Expression::NewTarget(_) => {
                self.emit("new");
                self.emit("target");
            }
            _ => walk_expression(self, it),
        }
    }

    fn visit_statement(&mut self, it: &Statement<'a>) {
        if !self.stopped {
            walk_statement(self, it);
        }
    }

    fn visit_function(&mut self, it: &Function<'a>, _flags: ScopeFlags) {
        if it.r#type != FunctionType::FunctionDeclaration || !is_root(self.root, it) {
            return;
        }

        self.scoped(&params_declared(it), |walker| {
            walker.params(&it.params);
            if let Some(body) = &it.body {
                walker.visit_function_body(body);
            }
        });
    }

    fn visit_arrow_function_expression(&mut self, _it: &ArrowFunctionExpression<'a>) {}

    fn visit_function_body(&mut self, it: &FunctionBody<'a>) {
        self.scoped(&body_declared(&it.statements), |walker| {
            walk_function_body(walker, it);
        });
    }

    fn visit_block_statement(&mut self, it: &BlockStatement<'a>) {
        self.scoped(&body_declared(&it.body), |walker| {
            walk_block_statement(walker, it);
        });
    }

    fn visit_static_block(&mut self, it: &StaticBlock<'a>) {
        self.scoped(&body_declared(&it.body), |walker| {
            walk_static_block(walker, it);
        });
    }

    fn visit_for_statement(&mut self, it: &ForStatement<'a>) {
        self.scoped(&for_declared(it.init.as_ref()), |walker| {
            walk_for_statement(walker, it);
        });
    }

    fn visit_for_in_statement(&mut self, it: &ForInStatement<'a>) {
        self.scoped(&left_declared(&it.left), |walker| {
            walk_for_in_statement(walker, it);
        });
    }

    fn visit_for_of_statement(&mut self, it: &ForOfStatement<'a>) {
        self.scoped(&left_declared(&it.left), |walker| {
            walk_for_of_statement(walker, it);
        });
    }

    fn visit_switch_statement(&mut self, it: &SwitchStatement<'a>) {
        self.scoped(&switch_declared(it), |walker| {
            walk_switch_statement(walker, it);
        });
    }

    fn visit_catch_clause(&mut self, it: &CatchClause<'a>) {
        self.scoped(&catch_declared(it), |walker| {
            if let Some(param) = &it.param {
                walker.with_binding(true, |walker| walker.visit_binding_pattern(&param.pattern));
            }

            walker.visit_block_statement(&it.body);
        });
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        if let Some(init) = &it.init {
            self.visit_expression(init);
        }
    }

    fn visit_assignment_pattern(&mut self, it: &AssignmentPattern<'a>) {
        self.visit_binding_pattern(&it.left);
        self.with_binding(false, |walker| walker.visit_expression(&it.right));
    }

    fn visit_binding_property(&mut self, it: &BindingProperty<'a>) {
        if it.computed {
            self.with_binding(false, |walker| walker.visit_property_key(&it.key));
        }

        self.visit_binding_pattern(&it.value);
    }

    fn visit_class(&mut self, it: &Class<'a>) {
        self.visit_decorators(&it.decorators);
        if let Some(heritage) = &it.heritage {
            self.visit_expression(&heritage.expression);
        }

        self.visit_ts_class_implements_list(&it.implements);
        walk_class_body(self, &it.body);
    }

    fn visit_method_definition(&mut self, it: &MethodDefinition<'a>) {
        if it.r#type != MethodDefinitionType::TSAbstractMethodDefinition {
            self.visit_decorators(&it.decorators);
            self.visit_property_key(&it.key);
        }
    }

    fn visit_property_definition(&mut self, it: &PropertyDefinition<'a>) {
        if it.r#type == PropertyDefinitionType::TSAbstractPropertyDefinition {
            return;
        }

        self.visit_decorators(&it.decorators);
        self.visit_property_key(&it.key);
        if let Some(value) = &it.value {
            self.visit_expression(value);
        }
    }

    fn visit_accessor_property(&mut self, it: &AccessorProperty<'a>) {
        if it.r#type == AccessorPropertyType::TSAbstractAccessorProperty {
            return;
        }

        self.visit_decorators(&it.decorators);
        self.visit_property_key(&it.key);
        if let Some(value) = &it.value {
            self.visit_expression(value);
        }
    }

    fn visit_jsx_opening_element(&mut self, it: &JSXOpeningElement<'a>) {
        if let Some(path) = jsx_element_path(&it.name) {
            self.emit(path[0]);
        }

        self.visit_jsx_attribute_items(&it.attributes);
    }

    fn visit_jsx_closing_element(&mut self, _it: &JSXClosingElement<'a>) {}

    fn visit_switch_case(&mut self, it: &SwitchCase<'a>) {
        if !self.stopped {
            walk_switch_case(self, it);
        }
    }

    fn visit_ts_index_signature(&mut self, _it: &TSIndexSignature<'a>) {}

    fn visit_ts_type(&mut self, _it: &TSType<'a>) {}

    fn visit_ts_type_annotation(&mut self, _it: &TSTypeAnnotation<'a>) {}

    fn visit_ts_type_parameter_declaration(&mut self, _it: &TSTypeParameterDeclaration<'a>) {}

    fn visit_ts_type_parameter_instantiation(&mut self, _it: &TSTypeParameterInstantiation<'a>) {}

    fn visit_ts_this_parameter(&mut self, _it: &TSThisParameter<'a>) {}

    fn visit_ts_type_alias_declaration(&mut self, _it: &TSTypeAliasDeclaration<'a>) {}

    fn visit_ts_interface_declaration(&mut self, _it: &TSInterfaceDeclaration<'a>) {}

    fn visit_ts_enum_declaration(&mut self, _it: &TSEnumDeclaration<'a>) {}

    fn visit_ts_external_module_declaration(&mut self, _it: &TSExternalModuleDeclaration<'a>) {}

    fn visit_ts_namespace_declaration(&mut self, _it: &TSNamespaceDeclaration<'a>) {}

    fn visit_ts_global_declaration(&mut self, _it: &TSGlobalDeclaration<'a>) {}

    fn visit_ts_import_equals_declaration(&mut self, _it: &TSImportEqualsDeclaration<'a>) {}

    fn visit_ts_namespace_export_declaration(&mut self, _it: &TSNamespaceExportDeclaration<'a>) {}
}

pub fn references(inner: Inner, names: &[&str], each: &mut dyn FnMut(&str) -> ControlFlow<()>) {
    if names.is_empty() {
        return;
    }

    let mut walker = References {
        visible: names.to_vec(),
        binding: false,
        root: root_address(inner),
        each,
        stopped: false,
    };

    walk_inner(&mut walker, inner);
}
