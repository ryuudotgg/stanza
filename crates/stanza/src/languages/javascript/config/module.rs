use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::rc::Rc;

use indexmap::IndexMap;
use oxc_allocator::UnstableAddress;
use oxc_ast::AstKind;
use oxc_ast::ast::{
    AssignmentExpression, AssignmentOperator, AssignmentTarget, BindingPattern, Declaration,
    Expression, ImportDeclarationSpecifier, Program, Statement, UnaryOperator,
    VariableDeclarationKind,
};

use super::evaluate::{OnUnwind, Presets, Value};
use super::find::Loader;
use super::references::References;
use super::syntax::{
    self, each_node, export_name, identifier_name, key_name, member_name, required_source,
    root_name, target_root,
};

#[derive(Debug)]
pub struct Imported {
    pub source: String,
    pub name: String,
    pub loader: Loader,
}

#[derive(Debug, Default)]
pub struct Binding {
    pub node: Option<&'static Expression<'static>>,
    pub imported: Option<Imported>,
    pub local: Option<String>,
    pub mutable: Cell<bool>,
    pub resolved: Cell<bool>,
    pub value: RefCell<Value>,
}

impl Binding {
    fn node(node: Option<&'static Expression<'static>>, mutable: bool) -> Rc<Self> {
        Rc::new(Self {
            node,
            mutable: Cell::new(mutable),
            ..Self::default()
        })
    }

    fn imported(source: &str, name: &str, loader: Loader) -> Rc<Self> {
        Rc::new(Self {
            imported: Some(Imported {
                source: source.to_owned(),
                name: name.to_owned(),
                loader,
            }),
            ..Self::default()
        })
    }

    fn local(name: &str) -> Rc<Self> {
        Rc::new(Self {
            local: Some(name.to_owned()),
            ..Self::default()
        })
    }

    fn mutable() -> Rc<Self> {
        Self::node(None, true)
    }
}

pub struct Module {
    pub file: Rc<str>,
    pub text: String,
    pub program: Option<&'static Program<'static>>,
    pub yaml: bool,
    pub mutated: Cell<bool>,
    pub bindings: RefCell<HashMap<String, Rc<Binding>>>,
    pub exports: RefCell<IndexMap<String, Rc<Binding>>>,
    pub values: RefCell<HashMap<String, Value>>,
}

impl std::fmt::Debug for Module {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Module({})", self.file)
    }
}

#[derive(Default)]
pub struct Session {
    modules: RefCell<HashMap<String, Option<Rc<Module>>>>,
    pub presets: Presets,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Helper {
    Flatten,
    Identity,
    Ignore,
}

const HELPERS: [(&str, &str, Helper); 6] = [
    ("eslint/config", "defineConfig", Helper::Flatten),
    ("eslint/config", "globalIgnores", Helper::Ignore),
    ("eslint-define-config", "defineConfig", Helper::Flatten),
    ("typescript-eslint", "config", Helper::Flatten),
    ("@eslint/compat", "includeIgnoreFile", Helper::Ignore),
    ("oxlint", "defineConfig", Helper::Identity),
];

const MUTATING_OBJECT_METHODS: [&str; 4] = [
    "assign",
    "defineProperty",
    "defineProperties",
    "setPrototypeOf",
];

fn spread_dirt(aliases: &[(&'static str, &'static str)], dirty: &mut HashSet<&'static str>) {
    let mut linked: HashMap<&str, Vec<&'static str>> = HashMap::new();
    for &(left, right) in aliases {
        linked.entry(left).or_default().push(right);
        linked.entry(right).or_default().push(left);
    }

    let mut pending: Vec<&str> = linked
        .keys()
        .copied()
        .filter(|name| dirty.contains(name))
        .collect();

    while let Some(name) = pending.pop() {
        for &next in &linked[name] {
            if dirty.insert(next) {
                pending.push(next);
            }
        }
    }
}

fn export_target(target: &AssignmentTarget<'static>) -> Option<&'static str> {
    let member = target.as_member_expression()?;
    match identifier_name(member.object())? {
        "module" if member_name(member) == Some("exports") => Some("default"),
        "exports" => member_name(member),
        _ => None,
    }
}

fn required_call(expression: &Expression<'static>) -> Option<&'static str> {
    match expression {
        Expression::CallExpression(call) => required_source(call),
        _ => None,
    }
}

#[cfg(test)]
static PARSES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub fn parses(file: &str) -> usize {
    PARSES
        .lock()
        .unwrap()
        .iter()
        .filter(|parsed| *parsed == file)
        .count()
}

fn script_name(file: &str) -> bool {
    [".js", ".ts", ".cjs", ".cts", ".mjs", ".mts"]
        .iter()
        .any(|extension| file.ends_with(extension))
}

impl Session {
    pub fn helper(&self, node: &Expression<'_>, module: &Module) -> Option<Helper> {
        let member = node.as_member_expression();
        let local = member.map_or(node, |member| member.object());
        let binding = module
            .bindings
            .borrow()
            .get(identifier_name(local)?)
            .cloned()?;
        let imported = binding.imported.as_ref()?;
        if binding.mutable.get() {
            return None;
        }

        let name = match member {
            Some(member) => member_name(member)?,
            None => imported.name.as_str(),
        };

        if member.is_some() && imported.name != "default" && imported.name != "*" {
            return None;
        }

        HELPERS
            .iter()
            .find(|(source, helper, _)| *source == imported.source && *helper == name)
            .map(|(_, _, kind)| *kind)
    }

    fn index_module(&self, module: &Module, program: &'static Program<'static>) {
        let mut references = References::default();
        let mut aliases: Vec<(&'static str, &'static str)> = Vec::new();
        let mut assignments: IndexMap<&'static str, Vec<&'static AssignmentExpression<'static>>> =
            IndexMap::new();
        let mut bindings = module.bindings.borrow_mut();
        let mut exports = module.exports.borrow_mut();
        for statement in &program.body {
            let declaration = match statement {
                Statement::VariableDeclaration(declaration) => Some((declaration, false)),
                Statement::ExportDeclaration(export) => match &export.declaration {
                    Declaration::VariableDeclaration(declaration) => Some((declaration, true)),
                    _ => None,
                },
                _ => None,
            };

            if let Some((declaration, exporting)) = declaration {
                let constant = declaration.kind == VariableDeclarationKind::Const;
                for declarator in &declaration.declarations {
                    let init = declarator.init.as_ref();
                    let source = init
                        .filter(|_| constant)
                        .and_then(required_call)
                        .filter(|source| !source.is_empty());

                    if let Some(source) = source
                        && let BindingPattern::ObjectPattern(pattern) = &declarator.id
                    {
                        for property in &pattern.properties {
                            let imported = (!property.computed)
                                .then(|| key_name(&property.key))
                                .flatten()
                                .filter(|name| !name.is_empty());

                            if let (Some(imported), BindingPattern::BindingIdentifier(local)) =
                                (imported, &property.value)
                            {
                                bindings.insert(
                                    local.name.to_string(),
                                    Binding::imported(source, imported, Loader::Require),
                                );
                            }
                        }
                    }

                    let BindingPattern::BindingIdentifier(id) = &declarator.id else {
                        continue;
                    };

                    let name = id.name.as_str();
                    let binding = match source {
                        Some(source) => Binding::imported(source, "default", Loader::Require),
                        None => Binding::node(init, !constant),
                    };

                    bindings.insert(name.to_owned(), binding);
                    if exporting {
                        exports.insert(name.to_owned(), Binding::local(name));
                    }

                    if let Some(alias) = init.and_then(root_name).filter(|alias| !alias.is_empty())
                    {
                        aliases.push((name, alias));
                    }
                }
            }

            match statement {
                Statement::ImportDeclaration(import) => {
                    let source = import.source.value.as_str();
                    for specifier in import.specifiers.iter().flatten() {
                        let (imported, local) = match specifier {
                            ImportDeclarationSpecifier::ImportDefaultSpecifier(specifier) => {
                                ("default", &specifier.local)
                            }
                            ImportDeclarationSpecifier::ImportNamespaceSpecifier(specifier) => {
                                ("*", &specifier.local)
                            }
                            ImportDeclarationSpecifier::ImportSpecifier(specifier) => {
                                (export_name(&specifier.imported), &specifier.local)
                            }
                        };

                        bindings.insert(
                            local.name.to_string(),
                            Binding::imported(source, imported, Loader::Import),
                        );
                    }
                }
                Statement::ExportDefaultDeclaration(export) => {
                    exports.insert(
                        "default".to_owned(),
                        Binding::node(export.declaration.as_expression(), false),
                    );
                }
                Statement::ExportNamedDeclaration(export) => {
                    for specifier in &export.specifiers {
                        exports.insert(
                            export_name(&specifier.exported).to_owned(),
                            Binding::local(export_name(&specifier.local)),
                        );
                    }
                }
                Statement::ExportFromDeclaration(export) => {
                    for specifier in &export.specifiers {
                        exports.insert(
                            export_name(&specifier.exported).to_owned(),
                            Binding::mutable(),
                        );
                    }
                }
                Statement::ExpressionStatement(statement) => {
                    let Expression::AssignmentExpression(assignment) = &statement.expression else {
                        continue;
                    };

                    if let Some(name) = export_target(&assignment.left)
                        && assignment.operator == AssignmentOperator::Assign
                    {
                        assignments.entry(name).or_default().push(assignment);
                        exports.insert(
                            name.to_owned(),
                            Binding::node(Some(&assignment.right), false),
                        );
                    }
                }
                _ => {}
            }
        }

        drop(bindings);
        drop(exports);

        let permitted: HashSet<_> = assignments
            .values()
            .filter(|items| items.len() == 1)
            .flatten()
            .map(|assignment| assignment.unstable_address())
            .collect();

        let mut mutated = false;
        each_node(program, |kind| {
            let (root, member) = match kind {
                AstKind::AssignmentExpression(assignment) => {
                    if permitted.contains(&assignment.unstable_address()) {
                        return;
                    }

                    (
                        assignment
                            .left
                            .as_simple_assignment_target()
                            .and_then(target_root),
                        assignment.left.as_member_expression().is_some(),
                    )
                }
                AstKind::UpdateExpression(update) => (target_root(&update.argument), true),
                AstKind::UnaryExpression(unary) if unary.operator == UnaryOperator::Delete => (
                    root_name(&unary.argument),
                    unary.argument.is_member_expression(),
                ),
                AstKind::CallExpression(call) => {
                    let helped = self.helper(&call.callee, module).is_some();
                    if !helped && required_source(call).is_none_or(str::is_empty) {
                        references.scan(call.unstable_address(), &call.arguments);
                    }

                    let Some(callee) = call.callee.as_member_expression() else {
                        return;
                    };

                    let root = root_name(callee.object());
                    let method = member_name(callee).unwrap_or("");
                    if root == Some("Reflect")
                        || (root == Some("Object") && MUTATING_OBJECT_METHODS.contains(&method))
                    {
                        mutated = true;
                    }

                    (root.filter(|_| !helped), false)
                }
                AstKind::NewExpression(new) => {
                    if self.helper(&new.callee, module).is_none() {
                        references.scan(new.unstable_address(), &new.arguments);
                    }

                    return;
                }
                _ => return,
            };

            mutated |= member;
            references.dirty.extend(root);
        });

        let mut dirty = references.dirty;
        let bindings = module.bindings.borrow();
        let mut exports = module.exports.borrow_mut();
        if mutated {
            module.mutated.set(true);
            for binding in bindings.values().chain(exports.values()) {
                binding.mutable.set(true);
            }

            exports.insert("default".to_owned(), Binding::mutable());
            return;
        }

        spread_dirt(&aliases, &mut dirty);

        for name in &dirty {
            if let Some(binding) = bindings.get(*name) {
                binding.mutable.set(true);
            }
        }

        if dirty.contains("module") || dirty.contains("exports") {
            exports.insert("default".to_owned(), Binding::mutable());
        }
    }

    pub fn module_at(&self, file: &str) -> Option<Rc<Module>> {
        #[cfg(test)]
        assert!(!file.ends_with(crate::format::PANICS), "injected panic");

        let Ok(real) = fs::canonicalize(file) else {
            return None;
        };

        let file = real.to_string_lossy().into_owned();
        if let Some(cached) = self.modules.borrow().get(&file) {
            return cached.clone();
        }

        let Ok(bytes) = fs::read(&file) else {
            self.modules.borrow_mut().insert(file, None);
            return None;
        };

        #[cfg(test)]
        PARSES.lock().unwrap().push(file.clone());

        let text = String::from_utf8_lossy(&bytes).into_owned();
        let script = script_name(&file);
        let program = if script {
            syntax::parse(&file, &text)
        } else {
            syntax::parse(&format!("{file}.js"), &format!("({text}\n)"))
        };

        let yaml = file.ends_with(".yaml")
            || file.ends_with(".yml")
            || (file.ends_with(".eslintrc") && program.is_none());

        let module = Rc::new(Module {
            file: file.as_str().into(),
            text,
            program,
            yaml,
            mutated: Cell::new(false),
            bindings: RefCell::default(),
            exports: RefCell::default(),
            values: RefCell::default(),
        });

        self.modules
            .borrow_mut()
            .insert(file.clone(), Some(module.clone()));
        let _unwound = OnUnwind(|| {
            if let Ok(mut modules) = self.modules.try_borrow_mut() {
                modules.remove(&file);
            }
        });

        if let Some(program) = program {
            if script {
                self.index_module(&module, program);
            } else if let Some(Statement::ExpressionStatement(statement)) = program.body.first() {
                module.exports.borrow_mut().insert(
                    "default".to_owned(),
                    Binding::node(Some(&statement.expression), false),
                );
            }
        }

        Some(module)
    }
}
