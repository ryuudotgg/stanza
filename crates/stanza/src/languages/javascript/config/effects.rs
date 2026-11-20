use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::rc::{Rc, Weak};

use oxc_ast::AstKind;
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, AssignmentTarget, AssignmentTargetMaybeDefault,
    AssignmentTargetProperty, CallExpression, ChainElement, Expression, ForStatementLeft,
    ImportDeclaration, ImportDeclarationSpecifier, ImportOrExportKind, MemberExpression,
    ObjectPropertyKind, Program, SimpleAssignmentTarget, TSModuleReference, UnaryOperator,
};

use super::evaluate::CHAIN_CAP;
use super::find::{Loader, is_local, resolve_module};
use super::module::{MUTATING_OBJECT_METHODS, Module, Session};
use super::syntax::{
    each_node, identifier_name, member_name, member_of, required_source, root_name,
};
use crate::languages::javascript::ast::pattern_names;
use crate::paths::{dirname, resolve};

pub const RUN_CAP: usize = 256;

const STORE_ROUNDS: usize = 4;

const LOCAL: u8 = 1;
const PACKAGE: u8 = 2;

const MUTATING_REFLECT_METHODS: [&str; 5] = [
    "set",
    "defineProperty",
    "deleteProperty",
    "setPrototypeOf",
    "preventExtensions",
];

const MUTATING_METHODS: [&str; 13] = [
    "add",
    "clear",
    "copyWithin",
    "delete",
    "fill",
    "pop",
    "push",
    "reverse",
    "set",
    "shift",
    "sort",
    "splice",
    "unshift",
];

const PURE_CALLBACKS: [&str; 3] = ["Boolean", "Number", "String"];

const CALLBACK_METHODS: [&str; 13] = [
    "every",
    "filter",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "flatMap",
    "forEach",
    "map",
    "reduce",
    "reduceRight",
    "some",
    "toSorted",
];

const READ_ONLY_METHODS: [&str; 23] = [
    "at",
    "concat",
    "every",
    "filter",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "flat",
    "flatMap",
    "forEach",
    "includes",
    "indexOf",
    "join",
    "lastIndexOf",
    "map",
    "reduce",
    "reduceRight",
    "slice",
    "some",
    "toReversed",
    "toSorted",
    "toSpliced",
];

#[derive(Default)]
struct Held {
    names: Vec<&'static str>,
    loads: u8,
    path: Option<Key>,
}

struct Flow {
    targets: Vec<&'static str>,
    from: Held,
}

struct Key {
    path: Vec<&'static str>,
    open: bool,
}

struct Passed {
    callee: Held,
    root: Option<&'static str>,
    arguments: Vec<Held>,
}

fn passed(
    callee: &'static Expression<'static>,
    arguments: &'static [Argument<'static>],
) -> Option<Passed> {
    if arguments.is_empty() {
        return None;
    }

    if let Some(member) = member_of(callee)
        && member_name(member).is_some_and(|method| {
            READ_ONLY_METHODS.contains(&method) || MUTATING_METHODS.contains(&method)
        })
    {
        return None;
    }

    Some(Passed {
        callee: held(callee),
        root: root_name(callee),
        arguments: arguments
            .iter()
            .map(|argument| match argument {
                Argument::SpreadElement(spread) => held(&spread.argument),
                argument => held(argument.to_expression()),
            })
            .collect(),
    })
}

struct Sink {
    held: Held,
    key: Option<Key>,
    member: bool,
    unless_global: Option<&'static str>,
}

#[derive(Default)]
struct Stored {
    at: HashMap<Vec<&'static str>, u8>,
    under: HashMap<Vec<&'static str>, u8>,
}

impl Stored {
    fn add(&mut self, key: &Key, kind: u8) {
        *self.at.entry(key.path.clone()).or_default() |= kind;
        for end in 1..=key.path.len() {
            *self.under.entry(key.path[..end].to_vec()).or_default() |= kind;
        }
    }

    fn kind(&self, key: &Key) -> u8 {
        let at = (1..=key.path.len())
            .filter_map(|end| self.at.get(&key.path[..end]))
            .fold(0, |kind, stored| kind | stored);

        if key.open {
            at | self.under.get(key.path.as_slice()).copied().unwrap_or(0)
        } else {
            at
        }
    }
}

fn member_key(member: &'static MemberExpression<'static>) -> Option<Key> {
    let mut properties = vec![member_name(member)];
    let mut object = member.object();
    while let Some(inner) = member_of(object) {
        properties.push(member_name(inner));
        object = inner.object();
    }

    let mut path = vec![identifier_name(object.get_inner_expression())?];
    if path[0] == "module" && properties.last() == Some(&Some("exports")) {
        path[0] = "exports";
        properties.pop();
    }

    let mut open = false;
    for property in properties.into_iter().rev() {
        let Some(property) = property else {
            open = true;
            break;
        };

        path.push(property);
    }

    Some(Key { path, open })
}

fn read_path(expression: &'static Expression<'static>) -> Option<Key> {
    let path = match expression.get_inner_expression() {
        Expression::Identifier(reference) => vec![reference.name.as_str()],
        inner => member_of(inner).and_then(member_key)?.path,
    };

    Some(Key { path, open: true })
}

fn key(expression: &'static Expression<'static>) -> Option<Key> {
    member_of(expression).and_then(member_key)
}

#[derive(Default)]
pub struct Effects {
    pub runs: Vec<(&'static str, Loader)>,
    pub writes: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Reach {
    pub writer: bool,
    pub height: usize,
}

const TAINTED: Reach = Reach {
    writer: true,
    height: 0,
};

fn runs_import(import: &ImportDeclaration<'_>) -> bool {
    import.import_kind != ImportOrExportKind::Type
        && import.specifiers.as_ref().is_none_or(|specifiers| {
            specifiers.is_empty()
                || specifiers.iter().any(|specifier| match specifier {
                    ImportDeclarationSpecifier::ImportSpecifier(specifier) => {
                        specifier.import_kind != ImportOrExportKind::Type
                    }
                    _ => true,
                })
        })
}

fn load_kind(source: &str) -> u8 {
    if is_local(source) { LOCAL } else { PACKAGE }
}

fn held(expression: &'static Expression<'static>) -> Held {
    let mut held = Held {
        path: read_path(expression),
        ..Held::default()
    };

    let mut pending = vec![expression];
    while let Some(expression) = pending.pop() {
        match expression.get_inner_expression() {
            Expression::Identifier(reference) => held.names.push(reference.name.as_str()),
            Expression::ChainExpression(chain) => {
                if let ChainElement::CallExpression(call) = &chain.expression
                    && let Some(callee) = member_of(&call.callee)
                    && member_name(callee).is_some_and(|method| READ_ONLY_METHODS.contains(&method))
                {
                    pending.push(callee.object());
                } else {
                    pending.extend(
                        chain
                            .expression
                            .as_member_expression()
                            .map(|member| member.object()),
                    );
                }
            }
            Expression::CallExpression(call) => {
                if let Some(source) = required_source(call).filter(|source| !source.is_empty()) {
                    held.loads |= load_kind(source);
                } else if let Some(callee) = member_of(&call.callee)
                    && member_name(callee).is_some_and(|method| READ_ONLY_METHODS.contains(&method))
                {
                    pending.push(callee.object());
                }
            }
            Expression::ImportExpression(import) => {
                if let Expression::StringLiteral(literal) = &import.source {
                    held.loads |= load_kind(&literal.value);
                }
            }
            Expression::AwaitExpression(awaited) => pending.push(&awaited.argument),
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
            Expression::AssignmentExpression(assignment) => {
                match assignment.left.as_simple_assignment_target() {
                    Some(SimpleAssignmentTarget::AssignmentTargetIdentifier(reference)) => {
                        held.names.push(reference.name.as_str());
                    }
                    _ => pending.push(&assignment.right),
                }
            }
            inner => pending.extend(member_of(inner).map(|member| member.object())),
        }
    }

    held
}

fn simple_parts(
    target: &'static SimpleAssignmentTarget<'static>,
    names: &mut Vec<&'static str>,
    members: &mut Vec<&'static Expression<'static>>,
) {
    if let SimpleAssignmentTarget::AssignmentTargetIdentifier(reference) = target {
        names.push(reference.name.as_str());
        return;
    }

    let member = match target.get_expression() {
        Some(inner) => member_of(inner),
        None => target.as_member_expression(),
    };

    members.extend(member.map(|member| member.object()));
}

fn target_parts(
    target: &'static AssignmentTarget<'static>,
    names: &mut Vec<&'static str>,
    members: &mut Vec<&'static Expression<'static>>,
) {
    let mut pending = vec![target];
    while let Some(target) = pending.pop() {
        if let Some(simple) = target.as_simple_assignment_target() {
            simple_parts(simple, names, members);
            continue;
        }

        let mut defaulted = |element: &'static AssignmentTargetMaybeDefault<'static>| match element
        {
            AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(it) => Some(&it.binding),
            element => element.as_assignment_target(),
        };

        let rest = match target {
            AssignmentTarget::ArrayAssignmentTarget(array) => {
                pending.extend(array.elements.iter().flatten().filter_map(&mut defaulted));
                &array.rest
            }
            AssignmentTarget::ObjectAssignmentTarget(object) => {
                for property in &object.properties {
                    match property {
                        AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(it) => {
                            names.push(it.binding.name.as_str());
                        }
                        AssignmentTargetProperty::AssignmentTargetPropertyProperty(it) => {
                            pending.extend(defaulted(&it.binding));
                        }
                    }
                }

                &object.rest
            }
            _ => continue,
        };

        pending.extend(rest.as_ref().map(|rest| &rest.target));
    }
}

enum Callback {
    Inline(Vec<&'static str>),
    Global(&'static str),
    Opaque,
}

fn callback(call: &'static CallExpression<'static>, method: &str) -> Option<Callback> {
    if !CALLBACK_METHODS.contains(&method) {
        return None;
    }

    let argument = call.arguments.first()?;
    let params = match argument
        .as_expression()
        .map(Expression::get_inner_expression)
    {
        Some(Expression::ArrowFunctionExpression(arrow)) => &arrow.params,
        Some(Expression::FunctionExpression(function)) => &function.params,
        Some(Expression::Identifier(reference))
            if PURE_CALLBACKS.contains(&reference.name.as_str()) =>
        {
            return Some(Callback::Global(reference.name.as_str()));
        }
        _ => return Some(Callback::Opaque),
    };

    let mut names = Vec::new();
    for param in &params.items {
        pattern_names(&param.pattern, &mut names);
    }

    if let Some(rest) = &params.rest {
        pattern_names(&rest.rest.argument, &mut names);
    }

    Some(Callback::Inline(names))
}

fn mutator_target(call: &'static CallExpression<'static>) -> bool {
    let Some(callee) = member_of(&call.callee) else {
        return false;
    };

    let method = member_name(callee).unwrap_or("");
    match root_name(callee.object()) {
        Some("Object") => MUTATING_OBJECT_METHODS.contains(&method),
        Some("Reflect") => MUTATING_REFLECT_METHODS.contains(&method),
        _ => false,
    }
}

// A loaded value can reach a name declared before it, as in a function body above the import, so every flow is collected before any write is judged.
pub fn effects(program: &'static Program<'static>) -> Effects {
    let mut effects = Effects::default();
    let mut opaque = false;
    let mut flows: Vec<Flow> = Vec::new();
    let mut sinks: Vec<Sink> = Vec::new();
    let mut declared = HashSet::new();
    let mut stores: Vec<(Key, Held)> = Vec::new();
    let mut calls: Vec<Passed> = Vec::new();
    each_node(program, |kind| {
        let mut members = Vec::new();
        if let AstKind::BindingIdentifier(binding) = kind {
            declared.insert(binding.name.as_str());
        }

        match kind {
            AstKind::ImportDeclaration(import) if runs_import(import) => {
                let source = import.source.value.as_str();
                if is_local(source) {
                    effects.runs.push((source, Loader::Import));
                }

                flows.push(Flow {
                    targets: import
                        .specifiers
                        .iter()
                        .flatten()
                        .map(|specifier| specifier.local().name.as_str())
                        .collect(),
                    from: Held {
                        loads: load_kind(source),
                        ..Held::default()
                    },
                });
            }
            AstKind::ExportAllDeclaration(export)
                if export.export_kind != ImportOrExportKind::Type =>
            {
                let source = export.source.value.as_str();
                if is_local(source) {
                    effects.runs.push((source, Loader::Import));
                }
            }
            AstKind::ExportFromDeclaration(export)
                if export.export_kind != ImportOrExportKind::Type =>
            {
                let source = export.source.value.as_str();
                if is_local(source) {
                    effects.runs.push((source, Loader::Import));
                }
            }
            AstKind::ImportExpression(import) => match &import.source {
                Expression::StringLiteral(literal) if is_local(&literal.value) => {
                    effects.runs.push((literal.value.as_str(), Loader::Import));
                }
                Expression::StringLiteral(_) => {}
                _ => opaque = true,
            },
            AstKind::VariableDeclarator(declarator) => {
                if let Some(init) = &declarator.init {
                    let mut targets = Vec::new();
                    pattern_names(&declarator.id, &mut targets);
                    flows.push(Flow {
                        targets,
                        from: held(init),
                    });
                }
            }
            AstKind::AssignmentPattern(pattern) => {
                let mut targets = Vec::new();
                pattern_names(&pattern.left, &mut targets);
                flows.push(Flow {
                    targets,
                    from: held(&pattern.right),
                });
            }
            AstKind::AssignmentTargetPropertyIdentifier(it) => {
                if let Some(init) = &it.init {
                    flows.push(Flow {
                        targets: vec![it.binding.name.as_str()],
                        from: held(init),
                    });
                }
            }
            AstKind::AssignmentTargetWithDefault(it) => {
                let mut targets = Vec::new();
                target_parts(&it.binding, &mut targets, &mut Vec::new());
                flows.push(Flow {
                    targets,
                    from: held(&it.init),
                });
            }
            AstKind::TSImportEqualsDeclaration(import)
                if import.import_kind != ImportOrExportKind::Type =>
            {
                if let TSModuleReference::ExternalModuleReference(reference) =
                    &import.module_reference
                {
                    let source = reference.expression.value.as_str();
                    if is_local(source) {
                        effects.runs.push((source, Loader::Require));
                    }

                    flows.push(Flow {
                        targets: vec![import.id.name.as_str()],
                        from: Held {
                            loads: load_kind(source),
                            ..Held::default()
                        },
                    });
                }
            }
            AstKind::FormalParameter(param) => {
                if let Some(initializer) = &param.initializer {
                    let mut targets = Vec::new();
                    pattern_names(&param.pattern, &mut targets);
                    flows.push(Flow {
                        targets,
                        from: held(initializer),
                    });
                }
            }
            AstKind::ForOfStatement(statement) => {
                let mut targets = Vec::new();
                match &statement.left {
                    ForStatementLeft::VariableDeclaration(declaration) => {
                        for declarator in &declaration.declarations {
                            pattern_names(&declarator.id, &mut targets);
                        }
                    }
                    left => {
                        if let Some(target) = left.as_assignment_target() {
                            target_parts(target, &mut targets, &mut members);
                        }
                    }
                }

                flows.push(Flow {
                    targets,
                    from: held(&statement.right),
                });
            }
            AstKind::ForInStatement(statement) => {
                if let Some(target) = statement.left.as_assignment_target() {
                    target_parts(target, &mut Vec::new(), &mut members);
                }
            }
            AstKind::AssignmentExpression(assignment) => {
                let mut targets = Vec::new();
                target_parts(&assignment.left, &mut targets, &mut members);
                if let Some(store) = assignment.left.as_member_expression().and_then(member_key) {
                    stores.push((store, held(&assignment.right)));
                }

                flows.push(Flow {
                    targets,
                    from: held(&assignment.right),
                });
            }
            AstKind::UpdateExpression(update) => {
                simple_parts(&update.argument, &mut Vec::new(), &mut members);
            }
            AstKind::UnaryExpression(unary) if unary.operator == UnaryOperator::Delete => {
                members.extend(member_of(&unary.argument).map(|member| member.object()));
            }
            AstKind::CallExpression(call) => {
                let requires = identifier_name(&call.callee) == Some("require");
                match required_source(call) {
                    Some(source) if is_local(source) => {
                        effects.runs.push((source, Loader::Require));
                    }
                    None if requires => opaque = true,
                    _ => {}
                }

                if required_source(call).is_none() {
                    calls.extend(passed(&call.callee, &call.arguments));
                }

                if mutator_target(call) {
                    match call.arguments.first() {
                        Some(Argument::SpreadElement(_)) => opaque = true,
                        first => members.extend(first.and_then(Argument::as_expression)),
                    }
                } else if let Some(callee) = member_of(&call.callee) {
                    let method = member_name(callee).unwrap_or("");
                    if READ_ONLY_METHODS.contains(&method) {
                        match callback(call, method) {
                            Some(Callback::Inline(targets)) => flows.push(Flow {
                                targets,
                                from: held(callee.object()),
                            }),
                            Some(Callback::Global(name)) => sinks.push(Sink {
                                held: held(callee.object()),
                                key: key(callee.object()),
                                member: true,
                                unless_global: Some(name),
                            }),
                            Some(Callback::Opaque) => sinks.push(Sink {
                                held: held(callee.object()),
                                key: key(callee.object()),
                                member: true,
                                unless_global: None,
                            }),
                            None => {}
                        }
                    } else {
                        sinks.push(Sink {
                            held: held(callee.object()),
                            key: key(callee.object()),
                            member: MUTATING_METHODS.contains(&method),
                            unless_global: None,
                        });
                    }
                }
            }
            AstKind::NewExpression(new) => calls.extend(passed(&new.callee, &new.arguments)),
            AstKind::TaggedTemplateExpression(tagged) => {
                if let Some(tag) = member_of(&tagged.tag) {
                    sinks.push(Sink {
                        held: held(tag.object()),
                        key: key(tag.object()),
                        member: false,
                        unless_global: None,
                    });
                }
            }
            _ => {}
        }

        sinks.extend(members.into_iter().map(|object| Sink {
            held: held(object),
            key: key(object),
            member: true,
            unless_global: None,
        }));
    });

    let mut rounds = 0;
    let (loaded, stored) = loop {
        let loaded = provenance(&flows);
        let mut stored = Stored::default();
        for (key, value) in &stores {
            let kind = value.names.iter().fold(value.loads, |kind, name| {
                kind | loaded.get(name).copied().unwrap_or(0)
            });

            stored.add(key, kind);
        }

        let mut grew = false;
        for flow in &mut flows {
            let read = flow.from.path.as_ref().map_or(0, |path| stored.kind(path));
            grew |= read & !flow.from.loads != 0;
            flow.from.loads |= read;
        }

        if !grew {
            break (loaded, stored);
        }

        rounds += 1;
        if rounds > STORE_ROUNDS {
            opaque = true;
            break (loaded, stored);
        }
    };

    let kind = |held: &Held| {
        held.names.iter().fold(held.loads, |kind, name| {
            kind | loaded.get(name).copied().unwrap_or(0)
        })
    };

    let local_call = |call: &Passed| {
        let declared_root = call.root.is_some_and(|root| {
            declared.contains(root) && loaded.get(root).copied().unwrap_or(0) & PACKAGE == 0
        });

        declared_root || kind(&call.callee) & LOCAL != 0
    };

    effects.writes = opaque
        || calls.iter().any(|call| {
            local_call(call) && call.arguments.iter().any(|argument| kind(argument) != 0)
        })
        || sinks.iter().any(|sink| {
            if sink
                .unless_global
                .is_some_and(|name| !declared.contains(name))
            {
                return false;
            }

            let kind = kind(&sink.held) | sink.key.as_ref().map_or(0, |key| stored.kind(key));
            if sink.member {
                kind != 0
            } else {
                kind & LOCAL != 0
            }
        });

    effects
}

fn provenance(flows: &[Flow]) -> HashMap<&'static str, u8> {
    let mut readers: HashMap<&'static str, Vec<usize>> = HashMap::new();
    for (index, flow) in flows.iter().enumerate() {
        for name in &flow.from.names {
            readers.entry(name).or_default().push(index);
        }
    }

    let mut loaded: HashMap<&'static str, u8> = HashMap::new();
    let mut pending: VecDeque<(usize, u8)> = flows
        .iter()
        .enumerate()
        .filter(|(_, flow)| flow.from.loads != 0)
        .map(|(index, flow)| (index, flow.from.loads))
        .collect();

    let mut spent = vec![0u8; flows.len()];
    while let Some((index, kind)) = pending.pop_front() {
        let kind = kind & !spent[index];
        if kind == 0 {
            continue;
        }

        spent[index] |= kind;
        for &target in &flows[index].targets {
            let known = loaded.entry(target).or_default();
            let added = kind & !*known;
            if added == 0 {
                continue;
            }

            *known |= added;
            let readers = readers.get(target).into_iter().flatten();
            pending.extend(readers.map(|&reader| (reader, added)));
        }
    }

    loaded
}

struct Visit {
    module: Rc<Module>,
    children: Vec<Rc<Module>>,
    next: usize,
}

impl Session {
    pub fn runs_writer(&self, module: &Rc<Module>, depth: usize) -> bool {
        if self.reachable(module) > RUN_CAP {
            return true;
        }

        let reach = self.reach(module);
        self.depth.enter_file(depth + reach.height);
        reach.writer || depth + reach.height > CHAIN_CAP
    }

    fn resolve_children(&self, module: &Module) -> Option<Vec<Weak<Module>>> {
        if module.writes_loaded.get() || (module.program.is_none() && module.data.is_none()) {
            return None;
        }

        let mut children = Vec::new();
        let mut seen = HashSet::new();
        for &(source, loader) in module.runs.borrow().iter() {
            let Some(file) = resolve_module(source, &module.file, loader) else {
                let path = resolve(dirname(&module.file), source);
                if Path::new(&path).exists() || Path::new(&format!("{path}.node")).exists() {
                    return None;
                }

                continue;
            };

            let child = self.module_at(&file)?;
            if seen.insert(child.file.clone()) {
                children.push(Rc::downgrade(&child));
                if children.len() > RUN_CAP {
                    break;
                }
            }
        }

        Some(children)
    }

    fn children<'m>(&self, module: &'m Module) -> Option<&'m [Weak<Module>]> {
        module
            .children
            .get_or_init(|| self.resolve_children(module))
            .as_deref()
    }

    fn reachable(&self, start: &Rc<Module>) -> usize {
        if let Some(known) = start.reachable.get() {
            return known;
        }

        let mut seen = HashSet::from([start.file.clone()]);
        let mut pending = vec![start.clone()];
        while let Some(module) = pending.pop() {
            for child in self.children(&module).unwrap_or_default() {
                let Some(child) = child.upgrade() else {
                    continue;
                };

                if !seen.insert(child.file.clone()) {
                    continue;
                }

                if seen.len() > RUN_CAP {
                    start.reachable.set(Some(RUN_CAP + 1));
                    return RUN_CAP + 1;
                }

                pending.push(child);
            }
        }

        start.reachable.set(Some(seen.len()));
        seen.len()
    }

    fn open(&self, module: &Rc<Module>) -> Option<Visit> {
        let children = self
            .children(module)?
            .iter()
            .filter_map(Weak::upgrade)
            .collect();

        Some(Visit {
            module: module.clone(),
            children,
            next: 0,
        })
    }

    // Components, not a walk per reader: every module of a cycle shares one answer, whichever module a reader opened first.
    fn reach(&self, start: &Rc<Module>) -> Reach {
        if let Some(known) = start.reach.get() {
            return known;
        }

        let reach = self.walk(start);
        if start.reach.get().is_none() {
            start.reach.set(Some(reach));
        }

        reach
    }

    fn walk(&self, start: &Rc<Module>) -> Reach {
        let Some(first) = self.open(start) else {
            return TAINTED;
        };

        let mut index: HashMap<Rc<str>, usize> = HashMap::from([(start.file.clone(), 0)]);
        let mut low = index.clone();
        let mut stack = vec![start.clone()];
        let mut visits = vec![first];
        let mut finished: HashMap<Rc<str>, Vec<Rc<Module>>> = HashMap::new();
        while let Some(top) = visits.last_mut() {
            if let Some(child) = top.children.get(top.next).cloned() {
                top.next += 1;
                if child.reach.get().is_some() {
                    continue;
                }

                let file = top.module.file.clone();
                if let Some(&seen) = index.get(&child.file) {
                    let lowest = low[&file].min(seen);
                    low.insert(file, lowest);
                    continue;
                }

                let Some(visit) = self.open(&child) else {
                    for visit in &visits {
                        visit.module.reach.set(Some(TAINTED));
                    }

                    return TAINTED;
                };

                let next = index.len();
                index.insert(child.file.clone(), next);
                low.insert(child.file.clone(), next);
                stack.push(child);
                visits.push(visit);
                continue;
            }

            let Some(done) = visits.pop() else {
                break;
            };

            let file = done.module.file.clone();
            finished.insert(file.clone(), done.children);
            if let Some(parent) = visits.last() {
                let lowest = low[&parent.module.file].min(low[&file]);
                low.insert(parent.module.file.clone(), lowest);
            }

            if low[&file] == index[&file] {
                let split = stack.iter().rposition(|member| member.file == file);
                let members = stack.split_off(split.unwrap_or_default());
                let reach = component(&members, &finished);
                for member in &members {
                    member.reach.set(Some(reach));
                }
            }
        }

        start.reach.get().unwrap_or(TAINTED)
    }
}

fn component(members: &[Rc<Module>], finished: &HashMap<Rc<str>, Vec<Rc<Module>>>) -> Reach {
    let inside: HashSet<&Rc<str>> = members.iter().map(|member| &member.file).collect();
    let children = |member: &Rc<Module>| finished.get(&member.file).into_iter().flatten();
    let mut below = Reach {
        writer: false,
        height: 0,
    };

    for member in members {
        for child in children(member) {
            if let Some(reach) = child.reach.get()
                && !inside.contains(&child.file)
            {
                below.writer |= reach.writer;
                below.height = below.height.max(reach.height);
            }
        }
    }

    let mut span = 0;
    if members.len() > 1 {
        for member in members {
            let mut seen = HashSet::from([member.file.clone()]);
            let mut level = vec![member];
            let mut depth = 0;
            while !level.is_empty() {
                span = span.max(depth);
                level = level
                    .into_iter()
                    .flat_map(children)
                    .filter(|child| inside.contains(&child.file) && seen.insert(child.file.clone()))
                    .collect();
                depth += 1;
            }
        }
    }

    Reach {
        writer: below.writer,
        height: below.height + span + 1,
    }
}
