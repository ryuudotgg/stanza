use std::collections::{HashMap, HashSet};

use oxc_allocator::{Address, UnstableAddress};
use oxc_ast::ast::{
    AccessorProperty, AccessorPropertyType, Argument, ArrowFunctionExpression, BindingIdentifier,
    BindingPattern, BlockStatement, CallExpression, CatchClause, Class, ClassType, ForInStatement,
    ForOfStatement, ForStatement, ForStatementInit, ForStatementLeft, FormalParameter,
    FormalParameterRest, FormalParameters, Function, FunctionBody, FunctionType,
    IdentifierReference, ImportMeta, JSXElementName, MethodDefinition, MethodDefinitionType,
    NewExpression, NewTarget, PropertyDefinition, PropertyDefinitionType, Statement, StaticBlock,
    SwitchStatement, TSEnumDeclaration, TSExternalModuleDeclaration, TSGlobalDeclaration,
    TSImportEqualsDeclaration, TSIndexSignature, TSInterfaceDeclaration, TSNamespaceDeclaration,
    TSNamespaceExportDeclaration, TSThisParameter, TSType, TSTypeAliasDeclaration,
    TSTypeAnnotation, TSTypeParameterDeclaration, TSTypeParameterInstantiation,
    VariableDeclaration, VariableDeclarationKind, VariableDeclarator,
};
use oxc_ast_visit::{Visit, walk};
use oxc_syntax::scope::ScopeFlags;

#[derive(Default)]
pub struct References {
    pub dirty: HashSet<&'static str>,
    covered: HashSet<Address>,
}

impl References {
    pub fn scan(&mut self, call: Address, arguments: &'static [Argument<'static>]) {
        if self.covered.contains(&call) {
            return;
        }

        for argument in arguments {
            Scan {
                references: self,
                shadowed: HashMap::new(),
            }
            .visit_argument(argument);
        }
    }
}

struct Scan<'r> {
    references: &'r mut References,
    shadowed: HashMap<&'static str, usize>,
}

impl Scan<'_> {
    fn reference(&mut self, name: &'static str) {
        if !self.shadowed.contains_key(name) {
            self.references.dirty.insert(name);
        }
    }

    fn scoped(&mut self, names: Vec<&'static str>, walk: impl FnOnce(&mut Self)) {
        for name in &names {
            *self.shadowed.entry(name).or_default() += 1;
        }

        walk(self);

        for name in names {
            if let Some(count) = self.shadowed.get_mut(name) {
                *count -= 1;
                if *count == 0 {
                    self.shadowed.remove(name);
                }
            }
        }
    }
}

impl Visit<'static> for Scan<'_> {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'static>) {
        self.reference(it.name.as_str());
    }

    fn visit_binding_identifier(&mut self, it: &BindingIdentifier<'static>) {
        self.reference(it.name.as_str());
    }

    fn visit_import_meta(&mut self, _: &ImportMeta) {
        self.reference("import");
        self.reference("meta");
    }

    fn visit_new_target(&mut self, _: &NewTarget) {
        self.reference("new");
        self.reference("target");
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'static>) {
        self.references.covered.insert(it.unstable_address());
        walk::walk_call_expression(self, it);
    }

    fn visit_new_expression(&mut self, it: &NewExpression<'static>) {
        self.references.covered.insert(it.unstable_address());
        walk::walk_new_expression(self, it);
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'static>) {
        if let Some(init) = &it.init {
            self.visit_expression(init);
        }
    }

    fn visit_function(&mut self, it: &Function<'static>, _flags: ScopeFlags) {
        let mut names = Vec::new();
        match it.r#type {
            FunctionType::FunctionDeclaration => {}
            FunctionType::FunctionExpression => {
                names.extend(it.id.as_ref().map(|id| id.name.as_str()));
                if let Some(body) = &it.body {
                    VarNames(&mut names).visit_function_body(body);
                }
            }
            FunctionType::TSDeclareFunction | FunctionType::TSEmptyBodyFunctionExpression => {
                return;
            }
        }

        param_names(&it.params, &mut names);
        self.scoped(names, |scan| {
            scan.visit_formal_parameters(&it.params);
            if let Some(body) = &it.body {
                scan.visit_function_body(body);
            }
        });
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'static>) {
        let mut names = Vec::new();
        param_names(&it.params, &mut names);
        VarNames(&mut names).visit_arrow_function_body(&it.body);

        self.scoped(names, |scan| {
            scan.visit_formal_parameters(&it.params);
            scan.visit_arrow_function_body(&it.body);
        });
    }

    fn visit_class(&mut self, it: &Class<'static>) {
        let names = match (&it.id, it.r#type) {
            (Some(id), ClassType::ClassExpression) => vec![id.name.as_str()],
            _ => Vec::new(),
        };

        self.scoped(names, |scan| {
            scan.visit_decorators(&it.decorators);
            if let Some(heritage) = &it.heritage {
                scan.visit_expression(&heritage.expression);
            }

            scan.visit_ts_class_implements_list(&it.implements);
            scan.visit_class_body(&it.body);
        });
    }

    fn visit_function_body(&mut self, it: &FunctionBody<'static>) {
        self.scoped(lexical_names(&it.statements), |scan| {
            walk::walk_function_body(scan, it);
        });
    }

    fn visit_block_statement(&mut self, it: &BlockStatement<'static>) {
        self.scoped(lexical_names(&it.body), |scan| {
            walk::walk_block_statement(scan, it);
        });
    }

    fn visit_static_block(&mut self, it: &StaticBlock<'static>) {
        self.scoped(lexical_names(&it.body), |scan| {
            walk::walk_static_block(scan, it);
        });
    }

    fn visit_switch_statement(&mut self, it: &SwitchStatement<'static>) {
        let names = it
            .cases
            .iter()
            .flat_map(|case| lexical_names(&case.consequent))
            .collect();

        self.scoped(names, |scan| walk::walk_switch_statement(scan, it));
    }

    fn visit_for_statement(&mut self, it: &ForStatement<'static>) {
        let names = match &it.init {
            Some(ForStatementInit::VariableDeclaration(declaration)) => {
                declaration_names(declaration)
            }
            _ => Vec::new(),
        };

        self.scoped(names, |scan| walk::walk_for_statement(scan, it));
    }

    fn visit_for_in_statement(&mut self, it: &ForInStatement<'static>) {
        self.scoped(left_names(&it.left), |scan| {
            walk::walk_for_in_statement(scan, it);
        });
    }

    fn visit_for_of_statement(&mut self, it: &ForOfStatement<'static>) {
        self.scoped(left_names(&it.left), |scan| {
            walk::walk_for_of_statement(scan, it);
        });
    }

    fn visit_catch_clause(&mut self, it: &CatchClause<'static>) {
        let mut names = Vec::new();
        if let Some(param) = &it.param {
            bound_names(&param.pattern, &mut names);
        }

        self.scoped(names, |scan| walk::walk_catch_clause(scan, it));
    }

    fn visit_formal_parameter(&mut self, it: &FormalParameter<'static>) {
        if it.has_modifier() {
            return;
        }

        // Decorators on a bare identifier parameter stay unscanned: ESTree hangs them on the Identifier, whose children the scan never enters.
        if it.initializer.is_some() || !matches!(it.pattern, BindingPattern::BindingIdentifier(_)) {
            self.visit_decorators(&it.decorators);
        }

        self.visit_binding_pattern(&it.pattern);
        if let Some(initializer) = &it.initializer {
            self.visit_expression(initializer);
        }
    }

    fn visit_formal_parameter_rest(&mut self, it: &FormalParameterRest<'static>) {
        self.visit_decorators(&it.decorators);
        self.visit_binding_rest_element(&it.rest);
    }

    fn visit_method_definition(&mut self, it: &MethodDefinition<'static>) {
        if it.r#type == MethodDefinitionType::MethodDefinition {
            walk::walk_method_definition(self, it);
        }
    }

    fn visit_property_definition(&mut self, it: &PropertyDefinition<'static>) {
        if it.r#type == PropertyDefinitionType::PropertyDefinition {
            walk::walk_property_definition(self, it);
        }
    }

    fn visit_accessor_property(&mut self, it: &AccessorProperty<'static>) {
        if it.r#type == AccessorPropertyType::AccessorProperty {
            walk::walk_accessor_property(self, it);
        }
    }

    fn visit_jsx_element_name(&mut self, _: &JSXElementName<'static>) {}

    fn visit_ts_type(&mut self, _: &TSType<'static>) {}

    fn visit_ts_type_annotation(&mut self, _: &TSTypeAnnotation<'static>) {}

    fn visit_ts_type_parameter_declaration(&mut self, _: &TSTypeParameterDeclaration<'static>) {}

    fn visit_ts_type_parameter_instantiation(&mut self, _: &TSTypeParameterInstantiation<'static>) {
    }

    fn visit_ts_this_parameter(&mut self, _: &TSThisParameter<'static>) {}

    fn visit_ts_index_signature(&mut self, _: &TSIndexSignature<'static>) {}

    fn visit_ts_type_alias_declaration(&mut self, _: &TSTypeAliasDeclaration<'static>) {}

    fn visit_ts_interface_declaration(&mut self, _: &TSInterfaceDeclaration<'static>) {}

    fn visit_ts_enum_declaration(&mut self, _: &TSEnumDeclaration<'static>) {}

    fn visit_ts_external_module_declaration(&mut self, _: &TSExternalModuleDeclaration<'static>) {}

    fn visit_ts_namespace_declaration(&mut self, _: &TSNamespaceDeclaration<'static>) {}

    fn visit_ts_global_declaration(&mut self, _: &TSGlobalDeclaration<'static>) {}

    fn visit_ts_import_equals_declaration(&mut self, _: &TSImportEqualsDeclaration<'static>) {}

    fn visit_ts_namespace_export_declaration(&mut self, _: &TSNamespaceExportDeclaration<'static>) {
    }
}

pub fn bound_names(pattern: &BindingPattern<'static>, names: &mut Vec<&'static str>) {
    match pattern {
        BindingPattern::BindingIdentifier(id) => names.push(id.name.as_str()),
        BindingPattern::ObjectPattern(object) => {
            for property in &object.properties {
                bound_names(&property.value, names);
            }

            if let Some(rest) = &object.rest {
                bound_names(&rest.argument, names);
            }
        }
        BindingPattern::ArrayPattern(array) => {
            for element in array.elements.iter().flatten() {
                bound_names(element, names);
            }

            if let Some(rest) = &array.rest {
                bound_names(&rest.argument, names);
            }
        }
        BindingPattern::AssignmentPattern(assignment) => bound_names(&assignment.left, names),
    }
}

fn param_names(params: &FormalParameters<'static>, names: &mut Vec<&'static str>) {
    for param in params.items.iter().filter(|param| !param.has_modifier()) {
        bound_names(&param.pattern, names);
    }

    if let Some(rest) = &params.rest {
        bound_names(&rest.rest.argument, names);
    }
}

fn declaration_names(declaration: &VariableDeclaration<'static>) -> Vec<&'static str> {
    let mut names = Vec::new();
    if declaration.kind != VariableDeclarationKind::Var {
        for declarator in &declaration.declarations {
            bound_names(&declarator.id, &mut names);
        }
    }

    names
}

fn left_names(left: &ForStatementLeft<'static>) -> Vec<&'static str> {
    match left {
        ForStatementLeft::VariableDeclaration(declaration) => declaration_names(declaration),
        _ => Vec::new(),
    }
}

fn lexical_names(statements: &[Statement<'static>]) -> Vec<&'static str> {
    let mut names = Vec::new();
    for statement in statements {
        match statement {
            Statement::VariableDeclaration(declaration) => {
                names.extend(declaration_names(declaration));
            }
            Statement::FunctionDeclaration(function)
                if function.r#type == FunctionType::FunctionDeclaration =>
            {
                names.extend(function.id.as_ref().map(|id| id.name.as_str()));
            }
            Statement::ClassDeclaration(class) => {
                names.extend(class.id.as_ref().map(|id| id.name.as_str()));
            }
            _ => {}
        }
    }

    names
}

struct VarNames<'n>(&'n mut Vec<&'static str>);

impl Visit<'static> for VarNames<'_> {
    fn visit_function(&mut self, _: &Function<'static>, _: ScopeFlags) {}

    fn visit_arrow_function_expression(&mut self, _: &ArrowFunctionExpression<'static>) {}

    fn visit_variable_declaration(&mut self, it: &VariableDeclaration<'static>) {
        if it.kind == VariableDeclarationKind::Var {
            for declarator in &it.declarations {
                bound_names(&declarator.id, self.0);
            }
        }

        walk::walk_variable_declaration(self, it);
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::super::evaluate::Value;
    use super::super::module::Session;
    use super::super::tests::dir_with;

    fn known(file: &str, text: &str) -> Vec<Option<f64>> {
        let scratch = dir_with(&[(file, text)]);
        let value = Session::default().exported(&scratch.join(file), "default", &Rc::default());
        let Value::Array(items) = value else {
            panic!("{file} exports an array: {value:?}");
        };

        items
            .iter()
            .map(|item| match item {
                Value::Number(number) => Some(*number),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn call_arguments_dirty_the_names_estree_counted_as_references() {
        let script = "const n1 = 1, n2 = 2, n3 = 3, n4 = 4, n5 = 5, n6 = 6, n7 = 7, n8 = 8, n9 = 9, n10 = 10, n11 = 11, n12 = 12, n13 = 13, n14 = 14, n15 = 15, n16 = 16, n17 = 17, n18 = 18, n19 = 19, meta = 20, target = 21, Foo = 22, Bar = 23;
f((n1) => n1);
f(function n2() { return n2; });
f(() => { var n3; return n3; });
f(() => { try {} catch (n4) { return n4; } });
f(() => { for (const n5 of []) { n5; } });
f(() => { switch (0) { case 0: let n6; } return n6; });
f(class n7 { m() { return n7; } });
f(class { static { let n8; n8; } });
f(() => { function g(n9) { var n10; } n10; n9; });
f({ n11: 1, [n12]: 2 });
f(a.n13, a[n14]);
f(() => { function g() { var n15; n15 = 1; } });
f(({ n16 }) => n16, ([n17 = n18]) => n17);
f((n19) => g(n19));
function h() { f(new.target, import.meta); }
f(<Foo.Bar />, <Bar />);
export default [n1, n2, n3, n4, n5, n6, n7, n8, n9, n10, n11, n12, n13, n14, n15, n16, n17, n18, n19, meta, target, Foo, Bar];";

        let clean = [1, 2, 3, 4, 5, 7, 8, 11, 13, 16, 17, 19, 22, 23];

        let expected: Vec<Option<f64>> = (1..=23)
            .map(|n| clean.contains(&n).then_some(f64::from(n)))
            .collect();

        assert_eq!(known("eslint.config.js", script), expected);
    }

    #[test]
    fn typescript_arguments_skip_the_nodes_estree_kept_out_of_the_scan() {
        let script = "const meta = 1, target = 2, x = 3, I = 4, J = 5, P = 6, Q = 7, A1 = 8, A2 = 9, A3 = 10, K1 = 11, K2 = 12;
f(class implements I, J.K { constructor(private p = P, q = Q) {} });
f(x as typeof meta, <typeof target>x);
f(class { m(@A1 a, @A2 b = 1, @A3 { c }) {} });
f(() => { abstract class C { abstract [K1]: T; [K2]: T } });
export default [meta, target, x, I, J, P, Q, A1, A2, A3, K1, K2];";

        let clean = [1, 2, 6, 8, 11];
        let expected: Vec<Option<f64>> = (1..=12)
            .map(|n| clean.contains(&n).then_some(f64::from(n)))
            .collect();

        assert_eq!(known("eslint.config.ts", script), expected);
    }
}
