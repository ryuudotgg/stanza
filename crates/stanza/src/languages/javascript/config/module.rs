use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::rc::Rc;

use indexmap::IndexMap;

use super::estree::{self, Node};
use super::evaluate::{OnUnwind, Presets, Value};
use super::find::Loader;

#[derive(Debug)]
pub struct Imported {
    pub source: String,
    pub name: String,
    pub loader: Loader,
}

#[derive(Debug, Default)]
pub struct Binding {
    pub node: Option<Node>,
    pub imported: Option<Imported>,
    pub local: Option<String>,
    pub mutable: Cell<bool>,
    pub resolved: Cell<bool>,
    pub value: RefCell<Value>,
}

impl Binding {
    fn node(node: Option<Node>, mutable: bool) -> Rc<Self> {
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
    pub program: Option<Node>,
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

pub fn name_of(node: Node) -> Option<&'static str> {
    match node.kind() {
        "Identifier" => node.text("name"),
        "Literal" => node.text("value"),
        _ => None,
    }
}

pub fn member_name(node: Node) -> Option<&'static str> {
    let property = node.field("property")?;
    (node.kind() == "MemberExpression" && (!node.flag("computed") || property.kind() == "Literal"))
        .then(|| name_of(property))
        .flatten()
}

fn root_name(node: Node) -> Option<&'static str> {
    match node.kind() {
        "MemberExpression" => root_name(node.field("object")?),
        "Identifier" => node.text("name"),
        _ => None,
    }
}

fn identifier_name(node: Node) -> Option<&'static str> {
    (node.kind() == "Identifier")
        .then(|| node.text("name"))
        .flatten()
}

fn bound_names(root: Node) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "Identifier" => {
                if let Some(name) = node.text("name") {
                    names.insert(name.to_owned());
                }
            }
            "VariableDeclaration" => stack.extend(
                node.items("declarations")
                    .rev()
                    .filter_map(|declaration| declaration.field("id")),
            ),
            "ObjectPattern" => stack.extend(node.items("properties").rev()),
            "ArrayPattern" => stack.extend(node.items("elements").rev()),
            "Property" => stack.extend(node.field("value")),
            "AssignmentPattern" => stack.extend(node.field("left")),
            "RestElement" => stack.extend(node.field("argument")),
            _ => {}
        }
    }

    names
}

fn reference_child(node: Node, key: &str) -> bool {
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

    if node.kind().starts_with("TS") {
        return key == "expression";
    }

    if key == "key" || (node.kind() == "MemberExpression" && key == "property") {
        return node.flag("computed");
    }

    true
}

fn lexical_names(node: Node) -> Vec<String> {
    match node.kind() {
        "VariableDeclaration" if node.text("kind") == Some("var") => Vec::new(),
        "VariableDeclaration" => bound_names(node).into_iter().collect(),
        "FunctionDeclaration" | "ClassDeclaration" => node
            .field("id")
            .and_then(|id| id.text("name"))
            .map(str::to_owned)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn declared(node: Node) -> Vec<String> {
    match node.kind() {
        "ForStatement" => node.field("init").map(lexical_names).unwrap_or_default(),
        "ForInStatement" | "ForOfStatement" => {
            node.field("left").map(lexical_names).unwrap_or_default()
        }
        "BlockStatement" | "StaticBlock" => node.items("body").flat_map(lexical_names).collect(),
        "SwitchStatement" => node
            .items("cases")
            .flat_map(|clause| clause.items("consequent").flat_map(lexical_names))
            .collect(),
        "CatchClause" => node
            .field("param")
            .map(|param| bound_names(param).into_iter().collect())
            .unwrap_or_default(),
        "FunctionDeclaration" => params_names(node),
        _ => Vec::new(),
    }
}

fn params_names(node: Node) -> Vec<String> {
    node.items("params").flat_map(bound_names).collect()
}

fn var_names(body: Node) -> Vec<String> {
    let mut names = Vec::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        if FUNCTION_TYPES.contains(&node.kind()) {
            continue;
        }

        if node.kind() == "VariableDeclaration" && node.text("kind") == Some("var") {
            names.extend(bound_names(node));
        }

        stack.extend(node.children().into_iter().map(|(_, child)| child));
    }

    names
}

fn scope_names(node: Node) -> Vec<String> {
    match node.kind() {
        "ClassExpression" => node
            .field("id")
            .and_then(|id| id.text("name"))
            .map(str::to_owned)
            .into_iter()
            .collect(),
        "FunctionExpression" | "ArrowFunctionExpression" => {
            let mut names: Vec<String> = Vec::new();
            if node.kind() == "FunctionExpression"
                && let Some(name) = node.field("id").and_then(|id| id.text("name"))
            {
                names.push(name.to_owned());
            }

            names.extend(params_names(node));
            names.extend(node.field("body").map(var_names).unwrap_or_default());
            names
        }
        _ => declared(node),
    }
}

enum Scan {
    Enter(Node),
    Leave(Vec<String>),
}

fn dirty_references(root: Node, dirty: &mut HashSet<String>, scanned: &mut HashSet<Node>) {
    let mut shadowed: HashMap<String, usize> = HashMap::new();
    let mut stack = vec![Scan::Enter(root)];
    while let Some(step) = stack.pop() {
        let node = match step {
            Scan::Enter(node) => node,
            Scan::Leave(names) => {
                for name in names {
                    if let Some(count) = shadowed.get_mut(&name) {
                        *count -= 1;
                        if *count == 0 {
                            shadowed.remove(&name);
                        }
                    }
                }

                continue;
            }
        };

        if !scanned.insert(node) {
            continue;
        }

        if node.kind() == "Identifier" {
            if let Some(name) = node.text("name")
                && !shadowed.contains_key(name)
            {
                dirty.insert(name.to_owned());
            }

            continue;
        }

        let names = scope_names(node);
        if !names.is_empty() {
            for name in &names {
                *shadowed.entry(name.clone()).or_default() += 1;
            }

            stack.push(Scan::Leave(names));
        }

        for (key, child) in node.children() {
            if reference_child(node, key) {
                stack.push(Scan::Enter(child));
            }
        }
    }
}

fn spread_dirt(aliases: &[(String, String)], dirty: &mut HashSet<String>) {
    let mut linked: HashMap<&str, Vec<&str>> = HashMap::new();
    for (left, right) in aliases {
        linked.entry(left).or_default().push(right);
        linked.entry(right).or_default().push(left);
    }

    let mut pending: Vec<&str> = linked
        .keys()
        .copied()
        .filter(|name| dirty.contains(*name))
        .collect();

    while let Some(name) = pending.pop() {
        for &next in &linked[name] {
            if dirty.insert(next.to_owned()) {
                pending.push(next);
            }
        }
    }
}

fn export_name(node: Node) -> Option<&'static str> {
    if node.kind() != "MemberExpression" {
        return None;
    }

    let object = node.field("object").and_then(identifier_name)?;
    match object {
        "module" if member_name(node) == Some("exports") => Some("default"),
        "exports" => member_name(node),
        _ => None,
    }
}

pub fn required_source(node: Node) -> Option<&'static str> {
    if node.kind() != "CallExpression" {
        return None;
    }

    let callee = node.field("callee")?;
    let argument = node.items("arguments").next()?;
    (identifier_name(callee) == Some("require") && argument.kind() == "Literal")
        .then(|| argument.text("value"))
        .flatten()
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
    pub fn helper(&self, node: Node, module: &Module) -> Option<Helper> {
        let member = node.kind() == "MemberExpression";
        let local = if member { node.field("object")? } else { node };
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

    fn index_module(&self, module: &Module, program: Node) {
        let mut dirty: HashSet<String> = HashSet::new();
        let mut scanned: HashSet<Node> = HashSet::new();
        let mut aliases: Vec<(String, String)> = Vec::new();
        let mut assignments: IndexMap<String, Vec<Node>> = IndexMap::new();
        let mut bindings = module.bindings.borrow_mut();
        let mut exports = module.exports.borrow_mut();
        for statement in program.items("body") {
            let exporting = statement.kind() == "ExportNamedDeclaration";
            let node = if exporting {
                statement.field("declaration")
            } else {
                Some(statement)
            };

            if let Some(node) = node.filter(|node| node.kind() == "VariableDeclaration") {
                let constant = node.text("kind") == Some("const");
                for declaration in node.items("declarations") {
                    let init = declaration.field("init");
                    let source = init
                        .filter(|_| constant)
                        .and_then(required_source)
                        .filter(|source| !source.is_empty());

                    let Some(id) = declaration.field("id") else {
                        continue;
                    };

                    if let Some(source) = source
                        && id.kind() == "ObjectPattern"
                    {
                        for property in id.items("properties") {
                            let imported =
                                if property.kind() == "Property" && !property.flag("computed") {
                                    property
                                        .field("key")
                                        .and_then(name_of)
                                        .filter(|name| !name.is_empty())
                                } else {
                                    None
                                };

                            let local = property.field("value").and_then(identifier_name);
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

            match statement.kind() {
                "ImportDeclaration" => {
                    let source = statement
                        .field("source")
                        .and_then(|source| source.text("value"))
                        .unwrap_or_default();

                    for specifier in statement.items("specifiers") {
                        let imported = match specifier.kind() {
                            "ImportDefaultSpecifier" => "default",
                            "ImportNamespaceSpecifier" => "*",
                            _ => specifier.field("imported").and_then(name_of).unwrap_or(""),
                        };

                        if let Some(local) = specifier
                            .field("local")
                            .and_then(|local| local.text("name"))
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
                        Binding::node(statement.field("declaration"), false),
                    );
                }
                "ExportNamedDeclaration" => {
                    let reexport = statement.field("source").is_some();
                    for specifier in statement.items("specifiers") {
                        let exported = specifier.field("exported").and_then(name_of).unwrap_or("");
                        let binding = if reexport {
                            Binding::mutable()
                        } else {
                            Binding::local(specifier.field("local").and_then(name_of))
                        };

                        exports.insert(exported.to_owned(), binding);
                    }
                }
                "ExpressionStatement" => {
                    let Some(assignment) = statement
                        .field("expression")
                        .filter(|expression| expression.kind() == "AssignmentExpression")
                    else {
                        continue;
                    };

                    let name = assignment.field("left").and_then(export_name);
                    if let Some(name) = name
                        && assignment.text("operator") == Some("=")
                    {
                        assignments
                            .entry(name.to_owned())
                            .or_default()
                            .push(assignment);
                        exports.insert(
                            name.to_owned(),
                            Binding::node(assignment.field("right"), false),
                        );
                    }
                }
                _ => {}
            }
        }

        drop(bindings);
        drop(exports);

        let permitted: HashSet<Node> = assignments
            .values()
            .filter(|items| items.len() == 1)
            .flatten()
            .copied()
            .collect();

        let mut mutated = false;
        program.walk(|node| {
            let target = match node.kind() {
                "AssignmentExpression" => node.field("left"),
                "UpdateExpression" => node.field("argument"),
                "UnaryExpression" if node.text("operator") == Some("delete") => {
                    node.field("argument")
                }
                _ => None,
            };

            if let Some(target) = target {
                if permitted.contains(&node) {
                    return;
                }

                if target.kind() == "MemberExpression" || node.kind() == "UpdateExpression" {
                    mutated = true;
                }

                if let Some(root) = root_name(target) {
                    dirty.insert(root.to_owned());
                }
            }

            let call = node.kind() == "CallExpression";
            if !call && node.kind() != "NewExpression" {
                return;
            }

            let Some(callee) = node.field("callee") else {
                return;
            };

            let helped = |node: Node| self.helper(node, module).is_some();

            if !helped(callee) && required_source(node).is_none_or(str::is_empty) {
                for argument in node.items("arguments") {
                    dirty_references(argument, &mut dirty, &mut scanned);
                }
            }

            if !call || callee.kind() != "MemberExpression" {
                return;
            }

            let root = callee.field("object").and_then(root_name);
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

        spread_dirt(&aliases, &mut dirty);

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
        #[cfg(test)]
        assert!(!file.ends_with(crate::format::PANICS), "injected panic");

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

        #[cfg(test)]
        PARSES.lock().unwrap().push(file.clone());

        let text = String::from_utf8_lossy(&bytes).into_owned();
        let script = script_name(&file);
        let program = if script {
            estree::parse(&file, &text)
        } else {
            estree::parse(&format!("{file}.js"), &format!("({text}\n)"))
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
            } else if let Some(statement) = program
                .items("body")
                .next()
                .filter(|statement| statement.kind() == "ExpressionStatement")
            {
                module.exports.borrow_mut().insert(
                    "default".to_owned(),
                    Binding::node(statement.field("expression"), false),
                );
            }
        }

        Some(module)
    }
}
