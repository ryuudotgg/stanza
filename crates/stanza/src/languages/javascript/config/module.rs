use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::rc::Rc;

use indexmap::IndexMap;

use super::estree::{self, Node, children, field, flag, items, kind, same, text, walk};
use super::evaluate::{Presets, Value};
use super::find::Loader;

#[derive(Debug)]
pub struct Imported {
    pub source: String,
    pub name: String,
    pub loader: Loader,
}

#[derive(Debug, Default)]
pub struct Binding {
    pub node: Option<&'static Node>,
    pub imported: Option<Imported>,
    pub local: Option<String>,
    pub mutable: Cell<bool>,
    pub resolved: Cell<bool>,
    pub value: RefCell<Value>,
}

impl Binding {
    fn node(node: Option<&'static Node>, mutable: bool) -> Rc<Self> {
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

    fn local(name: Option<&str>) -> Rc<Self> {
        Rc::new(Self {
            local: name.map(str::to_owned),
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
    pub program: Option<&'static Node>,
    pub yaml: bool,
    pub mutated: Cell<bool>,
    pub bindings: RefCell<HashMap<String, Rc<Binding>>>,
    pub exports: RefCell<IndexMap<String, Rc<Binding>>>,
    pub values: RefCell<HashMap<String, Value>>,
    pub infinite: HashSet<u64>,
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

const FUNCTION_TYPES: [&str; 3] = [
    "FunctionDeclaration",
    "FunctionExpression",
    "ArrowFunctionExpression",
];

const MUTATING_OBJECT_METHODS: [&str; 4] = [
    "assign",
    "defineProperty",
    "defineProperties",
    "setPrototypeOf",
];

pub fn name_of(node: &Node) -> Option<&str> {
    match kind(node) {
        "Identifier" => text(node, "name"),
        "Literal" => text(node, "value"),
        _ => None,
    }
}

pub fn member_name(node: &Node) -> Option<&str> {
    let property = field(node, "property")?;
    (kind(node) == "MemberExpression" && (!flag(node, "computed") || kind(property) == "Literal"))
        .then(|| name_of(property))
        .flatten()
}

fn root_name(node: &Node) -> Option<&str> {
    match kind(node) {
        "MemberExpression" => root_name(field(node, "object")?),
        "Identifier" => text(node, "name"),
        _ => None,
    }
}

fn identifier_name(node: &Node) -> Option<&str> {
    (kind(node) == "Identifier")
        .then(|| text(node, "name"))
        .flatten()
}

fn bound_names(root: &Node) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match kind(node) {
            "Identifier" => {
                if let Some(name) = text(node, "name") {
                    names.insert(name.to_owned());
                }
            }
            "VariableDeclaration" => stack.extend(
                items(node, "declarations")
                    .iter()
                    .rev()
                    .filter_map(|declaration| field(declaration, "id")),
            ),
            "ObjectPattern" => stack.extend(items(node, "properties").iter().rev()),
            "ArrayPattern" => stack.extend(
                items(node, "elements")
                    .iter()
                    .rev()
                    .filter(|element| !element.is_null()),
            ),
            "Property" => stack.extend(field(node, "value")),
            "AssignmentPattern" => stack.extend(field(node, "left")),
            "RestElement" => stack.extend(field(node, "argument")),
            _ => {}
        }
    }

    names
}

fn reference_child(node: &Node, key: &str) -> bool {
    if [
        "id",
        "label",
        "typeAnnotation",
        "typeParameters",
        "returnType",
    ]
    .contains(&key)
    {
        return false;
    }

    if kind(node).starts_with("TS") {
        return key == "expression";
    }

    if key == "key" || (kind(node) == "MemberExpression" && key == "property") {
        return flag(node, "computed");
    }

    true
}

fn lexical_names(node: &Node) -> Vec<String> {
    match kind(node) {
        "VariableDeclaration" if text(node, "kind") == Some("var") => Vec::new(),
        "VariableDeclaration" => bound_names(node).into_iter().collect(),
        "FunctionDeclaration" | "ClassDeclaration" => field(node, "id")
            .and_then(|id| text(id, "name"))
            .map(str::to_owned)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn declared(node: &Node) -> Vec<String> {
    match kind(node) {
        "ForStatement" => field(node, "init").map(lexical_names).unwrap_or_default(),
        "ForInStatement" | "ForOfStatement" => {
            field(node, "left").map(lexical_names).unwrap_or_default()
        }
        "BlockStatement" | "StaticBlock" => {
            items(node, "body").iter().flat_map(lexical_names).collect()
        }
        "SwitchStatement" => items(node, "cases")
            .iter()
            .flat_map(|clause| items(clause, "consequent").iter().flat_map(lexical_names))
            .collect(),
        "CatchClause" => field(node, "param")
            .map(|param| bound_names(param).into_iter().collect())
            .unwrap_or_default(),
        "FunctionDeclaration" => params_names(node),
        _ => Vec::new(),
    }
}

fn params_names(node: &Node) -> Vec<String> {
    items(node, "params").iter().flat_map(bound_names).collect()
}

fn var_names(body: &Node) -> Vec<String> {
    let mut names = Vec::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        if FUNCTION_TYPES.contains(&kind(node)) {
            continue;
        }

        if kind(node) == "VariableDeclaration" && text(node, "kind") == Some("var") {
            names.extend(bound_names(node));
        }

        stack.extend(children(node).into_iter().map(|(_, child)| child));
    }

    names
}

fn scope_names(node: &Node) -> Vec<String> {
    match kind(node) {
        "ClassExpression" => field(node, "id")
            .and_then(|id| text(id, "name"))
            .map(str::to_owned)
            .into_iter()
            .collect(),
        "FunctionExpression" | "ArrowFunctionExpression" => {
            let mut names: Vec<String> = Vec::new();
            if kind(node) == "FunctionExpression"
                && let Some(name) = field(node, "id").and_then(|id| text(id, "name"))
            {
                names.push(name.to_owned());
            }

            names.extend(params_names(node));
            names.extend(field(node, "body").map(var_names).unwrap_or_default());
            names
        }
        _ => declared(node),
    }
}

fn dirty_references(
    root: &'static Node,
    dirty: &mut HashSet<String>,
    scanned: &mut HashSet<*const Node>,
) {
    let mut stack: Vec<(&'static Node, Rc<HashSet<String>>)> = vec![(root, Rc::default())];
    while let Some((node, shadowed)) = stack.pop() {
        if !scanned.insert(std::ptr::from_ref(node)) {
            continue;
        }

        if kind(node) == "Identifier" {
            if let Some(name) = text(node, "name")
                && !shadowed.contains(name)
            {
                dirty.insert(name.to_owned());
            }

            continue;
        }

        let names = scope_names(node);
        let visible = if names.is_empty() {
            shadowed
        } else {
            let mut union = shadowed.as_ref().clone();
            union.extend(names);
            Rc::new(union)
        };

        for (key, child) in children(node) {
            if reference_child(node, key) {
                stack.push((child, visible.clone()));
            }
        }
    }
}

fn export_name(node: &Node) -> Option<&str> {
    if kind(node) != "MemberExpression" {
        return None;
    }

    let object = field(node, "object").and_then(identifier_name)?;
    match object {
        "module" if member_name(node) == Some("exports") => Some("default"),
        "exports" => member_name(node),
        _ => None,
    }
}

pub fn required_source(node: &Node) -> Option<&str> {
    if kind(node) != "CallExpression" {
        return None;
    }

    let callee = field(node, "callee")?;
    let argument = items(node, "arguments").first()?;
    (identifier_name(callee) == Some("require") && kind(argument) == "Literal")
        .then(|| text(argument, "value"))
        .flatten()
}

fn script_name(file: &str) -> bool {
    [".js", ".ts", ".cjs", ".cts", ".mjs", ".mts"]
        .iter()
        .any(|extension| file.ends_with(extension))
}

impl Session {
    pub fn helper(&self, node: &Node, module: &Module) -> Option<Helper> {
        let member = kind(node) == "MemberExpression";
        let local = if member { field(node, "object")? } else { node };
        let binding = module
            .bindings
            .borrow()
            .get(identifier_name(local)?)
            .cloned()?;
        let imported = binding.imported.as_ref()?;
        if binding.mutable.get() {
            return None;
        }

        let name = if member {
            member_name(node)?
        } else {
            imported.name.as_str()
        };

        if member && imported.name != "default" && imported.name != "*" {
            return None;
        }

        HELPERS
            .iter()
            .find(|(source, helper, _)| *source == imported.source && *helper == name)
            .map(|(_, _, kind)| *kind)
    }

    fn index_module(&self, module: &Module, program: &'static Node) {
        let mut dirty: HashSet<String> = HashSet::new();
        let mut scanned: HashSet<*const Node> = HashSet::new();
        let mut aliases: Vec<(String, String)> = Vec::new();
        let mut assignments: IndexMap<String, Vec<&'static Node>> = IndexMap::new();
        let mut bindings = module.bindings.borrow_mut();
        let mut exports = module.exports.borrow_mut();
        for statement in items(program, "body") {
            let exporting = kind(statement) == "ExportNamedDeclaration";
            let node = if exporting {
                field(statement, "declaration")
            } else {
                Some(statement)
            };

            if let Some(node) = node.filter(|node| kind(node) == "VariableDeclaration") {
                let constant = text(node, "kind") == Some("const");
                for declaration in items(node, "declarations") {
                    let init = field(declaration, "init");
                    let source = init
                        .filter(|_| constant)
                        .and_then(required_source)
                        .filter(|source| !source.is_empty());

                    let Some(id) = field(declaration, "id") else {
                        continue;
                    };

                    if let Some(source) = source
                        && kind(id) == "ObjectPattern"
                    {
                        for property in items(id, "properties") {
                            let imported =
                                if kind(property) == "Property" && !flag(property, "computed") {
                                    field(property, "key")
                                        .and_then(name_of)
                                        .filter(|name| !name.is_empty())
                                } else {
                                    None
                                };

                            let local = field(property, "value").and_then(identifier_name);
                            if let (Some(imported), Some(local)) = (imported, local) {
                                bindings.insert(
                                    local.to_owned(),
                                    Binding::imported(source, imported, Loader::Require),
                                );
                            }
                        }
                    }

                    let Some(name) = identifier_name(id) else {
                        continue;
                    };

                    let binding = match source {
                        Some(source) => Binding::imported(source, "default", Loader::Require),
                        None => Binding::node(init, !constant),
                    };

                    bindings.insert(name.to_owned(), binding);
                    if exporting {
                        exports.insert(name.to_owned(), Binding::local(Some(name)));
                    }

                    if let Some(alias) = init.and_then(root_name).filter(|alias| !alias.is_empty())
                    {
                        aliases.push((name.to_owned(), alias.to_owned()));
                    }
                }
            }

            match kind(statement) {
                "ImportDeclaration" => {
                    let source = field(statement, "source")
                        .and_then(|source| text(source, "value"))
                        .unwrap_or_default();

                    for specifier in items(statement, "specifiers") {
                        let imported = match kind(specifier) {
                            "ImportDefaultSpecifier" => "default",
                            "ImportNamespaceSpecifier" => "*",
                            _ => field(specifier, "imported").and_then(name_of).unwrap_or(""),
                        };

                        if let Some(local) =
                            field(specifier, "local").and_then(|local| text(local, "name"))
                        {
                            bindings.insert(
                                local.to_owned(),
                                Binding::imported(source, imported, Loader::Import),
                            );
                        }
                    }
                }
                "ExportDefaultDeclaration" => {
                    exports.insert(
                        "default".to_owned(),
                        Binding::node(field(statement, "declaration"), false),
                    );
                }
                "ExportNamedDeclaration" => {
                    let reexport = field(statement, "source").is_some();
                    for specifier in items(statement, "specifiers") {
                        let exported = field(specifier, "exported").and_then(name_of).unwrap_or("");
                        let binding = if reexport {
                            Binding::mutable()
                        } else {
                            Binding::local(field(specifier, "local").and_then(name_of))
                        };

                        exports.insert(exported.to_owned(), binding);
                    }
                }
                "ExpressionStatement" => {
                    let Some(assignment) = field(statement, "expression")
                        .filter(|expression| kind(expression) == "AssignmentExpression")
                    else {
                        continue;
                    };

                    let name = field(assignment, "left").and_then(export_name);
                    if let Some(name) = name
                        && text(assignment, "operator") == Some("=")
                    {
                        assignments
                            .entry(name.to_owned())
                            .or_default()
                            .push(assignment);
                        exports.insert(
                            name.to_owned(),
                            Binding::node(field(assignment, "right"), false),
                        );
                    }
                }
                _ => {}
            }
        }

        drop(bindings);
        drop(exports);

        let permitted: Vec<&Node> = assignments
            .values()
            .filter(|items| items.len() == 1)
            .flatten()
            .copied()
            .collect();

        let mut mutated = false;
        walk(program, |node| {
            let target = match kind(node) {
                "AssignmentExpression" => field(node, "left"),
                "UpdateExpression" => field(node, "argument"),
                "UnaryExpression" if text(node, "operator") == Some("delete") => {
                    field(node, "argument")
                }
                _ => None,
            };

            if let Some(target) = target {
                if permitted.iter().any(|allowed| same(allowed, node)) {
                    return;
                }

                if kind(target) == "MemberExpression" || kind(node) == "UpdateExpression" {
                    mutated = true;
                }

                if let Some(root) = root_name(target) {
                    dirty.insert(root.to_owned());
                }
            }

            let call = kind(node) == "CallExpression";
            if !call && kind(node) != "NewExpression" {
                return;
            }

            let Some(callee) = field(node, "callee") else {
                return;
            };

            let helped = |node: &Node| self.helper(node, module).is_some();

            if !helped(callee) && required_source(node).is_none_or(str::is_empty) {
                for argument in items(node, "arguments") {
                    dirty_references(argument, &mut dirty, &mut scanned);
                }
            }

            if !call || kind(callee) != "MemberExpression" {
                return;
            }

            let root = field(callee, "object").and_then(root_name);
            let method = member_name(callee).unwrap_or("");
            if root == Some("Reflect")
                || (root == Some("Object") && MUTATING_OBJECT_METHODS.contains(&method))
            {
                mutated = true;
            }

            if let Some(root) = root
                && !helped(callee)
            {
                dirty.insert(root.to_owned());
            }
        });

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

        for _ in 0..aliases.len() {
            for (left, right) in &aliases {
                if dirty.contains(left) || dirty.contains(right) {
                    dirty.insert(left.clone());
                    dirty.insert(right.clone());
                }
            }
        }

        for name in &dirty {
            if let Some(binding) = bindings.get(name) {
                binding.mutable.set(true);
            }
        }

        if dirty.contains("module") || dirty.contains("exports") {
            exports.insert("default".to_owned(), Binding::mutable());
        }
    }

    pub fn module_at(&self, file: &str) -> Option<Rc<Module>> {
        let Ok(real) = fs::canonicalize(file) else {
            self.modules.borrow_mut().insert(file.to_owned(), None);
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

        let text = String::from_utf8_lossy(&bytes).into_owned();
        let script = script_name(&file);
        let parsed = if script {
            estree::parse(&file, &text)
        } else {
            estree::parse(&format!("{file}.js"), &format!("({text}\n)"))
        };

        let yaml = file.ends_with(".yaml")
            || file.ends_with(".yml")
            || (file.ends_with(".eslintrc") && parsed.is_none());

        let (program, infinite) = parsed.map_or((None, HashSet::new()), |parsed| {
            (Some(parsed.root), parsed.infinite)
        });

        let module = Rc::new(Module {
            file: file.as_str().into(),
            text,
            program,
            yaml,
            mutated: Cell::new(false),
            bindings: RefCell::default(),
            exports: RefCell::default(),
            values: RefCell::default(),
            infinite,
        });

        self.modules.borrow_mut().insert(file, Some(module.clone()));

        if let Some(program) = program {
            if script {
                self.index_module(&module, program);
            } else if let Some(statement) = items(program, "body")
                .first()
                .filter(|statement| kind(statement) == "ExpressionStatement")
            {
                module.exports.borrow_mut().insert(
                    "default".to_owned(),
                    Binding::node(field(statement, "expression"), false),
                );
            }
        }

        Some(module)
    }
}
