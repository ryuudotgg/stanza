use std::collections::{HashMap, HashSet};

use oxc_allocator::{Address, UnstableAddress};
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, ArrowFunctionExpression, AssignmentExpression,
    AssignmentOperator, AssignmentTarget, AssignmentTargetMaybeDefault, AssignmentTargetProperty,
    BindingPattern, BlockStatement, CallExpression, CatchClause, Class, ClassType, Expression,
    ForInStatement, ForOfStatement, ForStatement, ForStatementLeft, FormalParameters, Function,
    FunctionBody, FunctionType, NewExpression, ObjectPropertyKind, Program, StaticBlock,
    SwitchStatement, TaggedTemplateExpression, VariableDeclarator,
};
use oxc_ast_visit::{Visit, walk};
use oxc_syntax::scope::ScopeFlags;

use super::references::VarNames;
use super::syntax::{root_name, target_write};
use crate::languages::javascript::ast::{
    body_declared, catch_declared, for_declared, left_declared, params_declared,
    skip_typescript_subtrees, switch_declared,
};

const MODULE_SCOPE: usize = 0;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Node {
    Binding(&'static str, usize),
    Site(usize),
    BindingCallers(&'static str, usize),
    SiteCallers(usize),
}

impl Node {
    fn callers(self) -> Option<Self> {
        match self {
            Self::Binding(name, scope) => Some(Self::BindingCallers(name, scope)),
            Self::Site(site) => Some(Self::SiteCallers(site)),
            Self::BindingCallers(..) | Self::SiteCallers(_) => None,
        }
    }
}

struct Alias {
    names: Vec<Node>,
    holders: Vec<Node>,
}

impl Alias {
    fn callers(&self) -> Option<Self> {
        let names = self
            .holders
            .iter()
            .map(|node| node.callers())
            .collect::<Option<_>>()?;
        let holders = self
            .names
            .iter()
            .map(|node| node.callers())
            .collect::<Option<_>>()?;
        Some(Self { names, holders })
    }
}

pub struct Calls<'c> {
    pub helped: &'c dyn Fn(&Expression<'static>) -> bool,
    pub required: &'c dyn Fn(&CallExpression<'static>) -> bool,
}

pub fn spread(program: &'static Program<'static>, calls: Calls, dirty: &mut HashSet<&'static str>) {
    let mut scan = Scan {
        calls,
        declared: HashMap::new(),
        scopes: MODULE_SCOPE,
        aliases: Vec::new(),
        changed: Vec::new(),
        inline: HashMap::new(),
        named: HashMap::new(),
    };

    scan.visit_program(program);

    let mut aliases = scan.aliases;
    let callers: Vec<Alias> = aliases.iter().filter_map(Alias::callers).collect();
    aliases.extend(callers);

    for node in &scan.changed {
        if let Node::Binding(name, MODULE_SCOPE) = *node {
            dirty.insert(name);
        }
    }

    let seeds = dirty
        .iter()
        .map(|&name| Node::Binding(name, MODULE_SCOPE))
        .chain(scan.changed);

    for name in reached(&aliases, seeds) {
        dirty.insert(name);
    }
}

fn reached(aliases: &[Alias], seeds: impl Iterator<Item = Node>) -> Vec<&'static str> {
    let mut groups: HashMap<Node, Vec<usize>> = HashMap::new();
    for (group, alias) in aliases.iter().enumerate() {
        for &name in &alias.names {
            groups.entry(name).or_default().push(group);
        }
    }

    let mut seen: HashSet<Node> = seeds.collect();
    let mut pending: Vec<Node> = seen.iter().copied().collect();
    let mut spent = vec![false; aliases.len()];
    let mut module_names = Vec::new();
    while let Some(node) = pending.pop() {
        for &group in groups.get(&node).into_iter().flatten() {
            if std::mem::replace(&mut spent[group], true) {
                continue;
            }

            for &holder in &aliases[group].holders {
                if let Node::Binding(name, MODULE_SCOPE) = holder {
                    module_names.push(name);
                }

                if seen.insert(holder) {
                    pending.push(holder);
                }
            }
        }
    }

    module_names
}

fn held_names(expression: &Expression<'static>, names: &mut Vec<&'static str>) {
    let mut pending = vec![expression];
    while let Some(expression) = pending.pop() {
        match expression.get_inner_expression() {
            Expression::Identifier(reference) => names.push(reference.name.as_str()),
            Expression::ArrayExpression(array) => {
                for element in &array.elements {
                    match element {
                        ArrayExpressionElement::SpreadElement(spread) => {
                            pending.push(&spread.argument);
                        }
                        element => pending.extend(element.as_expression()),
                    }
                }
            }
            Expression::ObjectExpression(object) => {
                for property in &object.properties {
                    pending.push(match property {
                        ObjectPropertyKind::ObjectProperty(property) => &property.value,
                        ObjectPropertyKind::SpreadProperty(spread) => &spread.argument,
                    });
                }
            }
            Expression::ConditionalExpression(conditional) => {
                pending.extend([&conditional.consequent, &conditional.alternate]);
            }
            Expression::LogicalExpression(logical) => {
                pending.extend([&logical.left, &logical.right]);
            }
            Expression::SequenceExpression(sequence) => pending.extend(sequence.expressions.last()),
            Expression::AssignmentExpression(assignment) => names.extend(assigned_name(assignment)),
            Expression::ChainExpression(chain) => {
                pending.extend(
                    chain
                        .expression
                        .as_member_expression()
                        .map(|member| member.object()),
                );
            }
            inner => pending.extend(inner.as_member_expression().map(|member| member.object())),
        }
    }
}

fn passed_value<'n>(mut expression: &'n Expression<'static>) -> &'n Expression<'static> {
    loop {
        expression = expression.get_inner_expression();
        match expression {
            Expression::SequenceExpression(sequence) => match sequence.expressions.last() {
                Some(last) => expression = last,
                None => return expression,
            },
            _ => return expression,
        }
    }
}

fn assigned_name(assignment: &AssignmentExpression<'static>) -> Option<&'static str> {
    match target_write(assignment.left.as_simple_assignment_target()?) {
        (Some(name), false) => Some(name),
        _ => None,
    }
}

fn identity_names(expression: &Expression<'static>, names: &mut Vec<&'static str>) {
    let mut pending = vec![expression];
    while let Some(expression) = pending.pop() {
        match expression.get_inner_expression() {
            Expression::Identifier(reference) => names.push(reference.name.as_str()),
            Expression::SequenceExpression(sequence) => pending.extend(sequence.expressions.last()),
            Expression::AssignmentExpression(assignment) => names.extend(assigned_name(assignment)),
            Expression::ConditionalExpression(conditional) => {
                pending.extend([&conditional.consequent, &conditional.alternate]);
            }
            Expression::LogicalExpression(logical) => {
                pending.extend([&logical.left, &logical.right]);
            }
            inner => pending.extend(inner.as_member_expression().map(|member| member.object())),
        }
    }
}

fn unshadowed(
    mut vars: Vec<&'static str>,
    params: &FormalParameters<'static>,
) -> Vec<&'static str> {
    let params: HashSet<_> = params_declared(params).into_iter().collect();
    vars.retain(|name| !params.contains(name));
    vars
}

struct Scan<'c> {
    calls: Calls<'c>,
    declared: HashMap<&'static str, Vec<usize>>,
    scopes: usize,
    aliases: Vec<Alias>,
    changed: Vec<Node>,
    inline: HashMap<Address, Node>,
    named: HashMap<Address, Node>,
}

impl Scan<'_> {
    fn binding(&self, name: &'static str) -> Node {
        let scope = self
            .declared
            .get(name)
            .and_then(|scopes| scopes.last())
            .copied()
            .unwrap_or(MODULE_SCOPE);

        Node::Binding(name, scope)
    }

    fn bindings(&self, names: Vec<&'static str>) -> Vec<Node> {
        names.into_iter().map(|name| self.binding(name)).collect()
    }

    fn held(&self, expression: &Expression<'static>) -> Vec<Node> {
        let mut names = Vec::new();
        held_names(expression, &mut names);
        self.bindings(names)
    }

    fn helped(&self, callee: &Expression<'static>) -> bool {
        (self.calls.helped)(callee)
            && root_name(callee)
                .is_none_or(|name| self.binding(name) == Node::Binding(name, MODULE_SCOPE))
    }

    fn link(&mut self, names: Vec<Node>, holders: Vec<Node>) {
        if !names.is_empty() && !holders.is_empty() {
            self.aliases.push(Alias { names, holders });
        }
    }

    fn site(&mut self, mut holders: Vec<Node>, parent: Option<Node>) -> Option<Node> {
        if holders.is_empty() {
            return parent;
        }

        let site = Node::Site(self.aliases.len());
        holders.extend(parent);
        self.aliases.push(Alias {
            names: vec![site],
            holders,
        });

        Some(site)
    }

    fn held_site(
        &mut self,
        expression: &Expression<'static>,
        parent: Option<Node>,
    ) -> Option<Node> {
        let holders = self.held(expression);
        self.site(holders, parent)
    }

    fn bind(&mut self, name: &'static str, parent: Option<Node>) {
        if let Some(parent) = parent {
            let name = self.binding(name);
            self.aliases.push(Alias {
                names: vec![name],
                holders: vec![parent],
            });
        }
    }

    fn pattern(&mut self, pattern: &BindingPattern<'static>, parent: Option<Node>) {
        match pattern {
            BindingPattern::BindingIdentifier(id) => self.bind(id.name.as_str(), parent),
            BindingPattern::ObjectPattern(object) => {
                for property in &object.properties {
                    self.pattern(&property.value, parent);
                }

                if let Some(rest) = &object.rest {
                    self.pattern(&rest.argument, parent);
                }
            }
            BindingPattern::ArrayPattern(array) => {
                for element in array.elements.iter().flatten() {
                    self.pattern(element, parent);
                }

                if let Some(rest) = &array.rest {
                    self.pattern(&rest.argument, parent);
                }
            }
            BindingPattern::AssignmentPattern(assignment) => {
                if let BindingPattern::BindingIdentifier(id) = &assignment.left {
                    self.own(&assignment.right, id.name.as_str());
                }

                let site = self.held_site(&assignment.right, parent);
                self.pattern(&assignment.left, site);
            }
        }
    }

    fn target(&mut self, target: &AssignmentTarget<'static>, parent: Option<Node>) {
        if let Some(simple) = target.as_simple_assignment_target() {
            if let (Some(root), false) = target_write(simple) {
                self.bind(root, parent);
            }

            return;
        }

        let rest = match target {
            AssignmentTarget::ArrayAssignmentTarget(array) => {
                for element in array.elements.iter().flatten() {
                    self.defaulted(element, parent);
                }

                &array.rest
            }
            AssignmentTarget::ObjectAssignmentTarget(object) => {
                for property in &object.properties {
                    match property {
                        AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(it) => {
                            let site = match &it.init {
                                Some(init) => self.held_site(init, parent),
                                None => parent,
                            };

                            self.bind(it.binding.name.as_str(), site);
                        }
                        AssignmentTargetProperty::AssignmentTargetPropertyProperty(it) => {
                            self.defaulted(&it.binding, parent);
                        }
                    }
                }

                &object.rest
            }
            _ => return,
        };

        if let Some(rest) = rest {
            self.target(&rest.target, parent);
        }
    }

    fn defaulted(&mut self, target: &AssignmentTargetMaybeDefault<'static>, parent: Option<Node>) {
        match target {
            AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(it) => {
                let site = self.held_site(&it.init, parent);
                self.target(&it.binding, site);
            }
            _ => {
                if let Some(target) = target.as_assignment_target() {
                    self.target(target, parent);
                }
            }
        }
    }

    fn own(&mut self, function: &Expression<'static>, owner: &'static str) {
        let owner = self.binding(owner);
        match function.get_inner_expression() {
            Expression::ArrowFunctionExpression(arrow) => {
                self.named.insert(arrow.unstable_address(), owner);
            }
            Expression::FunctionExpression(function) => {
                self.named.insert(function.unstable_address(), owner);
            }
            _ => {}
        }
    }

    fn scoped(&mut self, names: Vec<&'static str>, walk: impl FnOnce(&mut Self)) {
        self.scopes += 1;
        for name in &names {
            self.declared.entry(name).or_default().push(self.scopes);
        }

        walk(self);

        for name in names {
            if let Some(scopes) = self.declared.get_mut(name) {
                scopes.pop();
            }
        }
    }

    fn escape(&mut self, arguments: &[Argument<'static>]) {
        for argument in arguments {
            let expression = match argument {
                Argument::SpreadElement(spread) => &spread.argument,
                argument => argument.to_expression(),
            };

            let held = self.held(expression);
            self.changed.extend(held);
        }
    }

    fn receiver(&mut self, callee: &Expression<'static>) -> Option<Vec<Node>> {
        let member = callee.get_inner_expression().as_member_expression()?;
        let mut roots = Vec::new();
        identity_names(member.object(), &mut roots);
        let roots = self.bindings(roots);
        self.changed.extend(roots);

        Some(self.held(member.object()))
    }

    fn function(
        &mut self,
        function: Address,
        params: &FormalParameters<'static>,
        scope: Vec<&'static str>,
        walk: impl FnOnce(&mut Self),
    ) {
        let site = self.inline.remove(&function);
        let owner = self.named.remove(&function);
        self.scoped(scope, |scan| {
            for param in &params.items {
                let parent = match &param.initializer {
                    Some(initializer) => scan.held_site(initializer, site),
                    None => site,
                };

                scan.pattern(&param.pattern, parent);
            }

            if let Some(rest) = &params.rest {
                scan.pattern(&rest.rest.argument, site);
            }

            if let Some(callers) = owner.and_then(Node::callers) {
                let names = scan.bindings(params_declared(params));
                scan.link(names, vec![callers]);
            }

            walk(scan);
        });
    }
}

impl Visit<'static> for Scan<'_> {
    fn visit_call_expression(&mut self, it: &CallExpression<'static>) {
        if !self.helped(&it.callee) {
            if let Some(holders) = self.receiver(&it.callee)
                && let Some(site) = self.site(holders, None)
            {
                for argument in &it.arguments {
                    match argument.as_expression().map(passed_value) {
                        Some(Expression::ArrowFunctionExpression(arrow)) => {
                            self.inline.insert(arrow.unstable_address(), site);
                        }
                        Some(Expression::FunctionExpression(function)) => {
                            self.inline.insert(function.unstable_address(), site);
                        }
                        Some(Expression::Identifier(reference)) => {
                            let callback = self.binding(reference.name.as_str());
                            self.link(callback.callers().into_iter().collect(), vec![site]);
                        }
                        _ => {}
                    }
                }
            }

            if !(self.calls.required)(it) {
                self.escape(&it.arguments);
            }
        }

        walk::walk_call_expression(self, it);
    }

    fn visit_new_expression(&mut self, it: &NewExpression<'static>) {
        if !self.helped(&it.callee) {
            self.escape(&it.arguments);
        }

        walk::walk_new_expression(self, it);
    }

    fn visit_tagged_template_expression(&mut self, it: &TaggedTemplateExpression<'static>) {
        if !self.helped(&it.tag) {
            self.receiver(&it.tag);
            for expression in &it.quasi.expressions {
                let held = self.held(expression);
                self.changed.extend(held);
            }
        }

        walk::walk_tagged_template_expression(self, it);
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'static>) {
        let site = match &it.init {
            Some(init) => self.held_site(init, None),
            None => None,
        };

        self.pattern(&it.id, site);
        if let (BindingPattern::BindingIdentifier(id), Some(init)) = (&it.id, &it.init) {
            self.own(init, id.name.as_str());
        }

        walk::walk_variable_declarator(self, it);
    }

    fn visit_assignment_expression(&mut self, it: &AssignmentExpression<'static>) {
        if it.operator == AssignmentOperator::Assign || it.operator.is_logical() {
            let site = self.held_site(&it.right, None);
            self.target(&it.left, site);
            if let Some(simple) = it.left.as_simple_assignment_target()
                && let (Some(owner), false) = target_write(simple)
            {
                self.own(&it.right, owner);
            }
        }

        walk::walk_assignment_expression(self, it);
    }

    fn visit_function(&mut self, it: &Function<'static>, _flags: ScopeFlags) {
        let mut scope = Vec::new();
        match it.r#type {
            FunctionType::FunctionDeclaration => {
                if let Some(id) = &it.id {
                    let owner = self.binding(id.name.as_str());
                    self.named.insert(it.unstable_address(), owner);
                }
            }
            FunctionType::FunctionExpression => {
                scope.extend(it.id.as_ref().map(|id| id.name.as_str()));
            }
            FunctionType::TSDeclareFunction | FunctionType::TSEmptyBodyFunctionExpression => {
                return;
            }
        }

        let mut vars = Vec::new();
        if let Some(body) = &it.body {
            VarNames(&mut vars).visit_function_body(body);
        }

        let vars = unshadowed(vars, &it.params);

        scope.extend(params_declared(&it.params));
        self.function(it.unstable_address(), &it.params, scope, |scan| {
            scan.visit_formal_parameters(&it.params);
            scan.scoped(vars, |scan| {
                if let Some(body) = &it.body {
                    scan.visit_function_body(body);
                }
            });
        });
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'static>) {
        let mut vars = Vec::new();
        VarNames(&mut vars).visit_arrow_function_body(&it.body);
        let vars = unshadowed(vars, &it.params);
        self.function(
            it.unstable_address(),
            &it.params,
            params_declared(&it.params),
            |scan| {
                scan.visit_formal_parameters(&it.params);
                scan.scoped(vars, |scan| scan.visit_arrow_function_body(&it.body));
            },
        );
    }

    fn visit_class(&mut self, it: &Class<'static>) {
        let names = match (&it.id, it.r#type) {
            (Some(id), ClassType::ClassExpression) => vec![id.name.as_str()],
            _ => Vec::new(),
        };

        self.scoped(names, |scan| walk::walk_class(scan, it));
    }

    fn visit_function_body(&mut self, it: &FunctionBody<'static>) {
        self.scoped(body_declared(&it.statements), |scan| {
            walk::walk_function_body(scan, it);
        });
    }

    fn visit_block_statement(&mut self, it: &BlockStatement<'static>) {
        self.scoped(body_declared(&it.body), |scan| {
            walk::walk_block_statement(scan, it);
        });
    }

    fn visit_static_block(&mut self, it: &StaticBlock<'static>) {
        let mut scope = body_declared(&it.body);
        for statement in &it.body {
            VarNames(&mut scope).visit_statement(statement);
        }

        self.scoped(scope, |scan| {
            walk::walk_static_block(scan, it);
        });
    }

    fn visit_switch_statement(&mut self, it: &SwitchStatement<'static>) {
        self.visit_expression(&it.discriminant);
        self.scoped(switch_declared(it), |scan| {
            for case in &it.cases {
                scan.visit_switch_case(case);
            }
        });
    }

    fn visit_for_statement(&mut self, it: &ForStatement<'static>) {
        self.scoped(for_declared(it.init.as_ref()), |scan| {
            walk::walk_for_statement(scan, it);
        });
    }

    fn visit_for_in_statement(&mut self, it: &ForInStatement<'static>) {
        self.scoped(left_declared(&it.left), |scan| {
            walk::walk_for_in_statement(scan, it);
        });
    }

    fn visit_for_of_statement(&mut self, it: &ForOfStatement<'static>) {
        self.scoped(left_declared(&it.left), |scan| {
            let site = scan.held_site(&it.right, None);
            match &it.left {
                ForStatementLeft::VariableDeclaration(declaration) => {
                    for declarator in &declaration.declarations {
                        scan.pattern(&declarator.id, site);
                    }
                }
                left => {
                    if let Some(target) = left.as_assignment_target() {
                        scan.target(target, site);
                    }
                }
            }

            walk::walk_for_of_statement(scan, it);
        });
    }

    fn visit_catch_clause(&mut self, it: &CatchClause<'static>) {
        self.scoped(catch_declared(it), |scan| walk::walk_catch_clause(scan, it));
    }

    skip_typescript_subtrees!();
}
