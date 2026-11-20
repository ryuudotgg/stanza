use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::rc::{Rc, Weak};

use indexmap::IndexMap;
use oxc_allocator::UnstableAddress;
use oxc_ast::AstKind;
use oxc_ast::ast::{
    AssignmentExpression, AssignmentOperator, AssignmentTarget, BindingPattern, CallExpression,
    Declaration, Expression, ImportDeclarationSpecifier, Program, Statement, UnaryOperator,
    VariableDeclarationKind,
};

use super::aliases::{self, Calls};
use super::depth::{Answer, Depth};
use super::effects::{self, Reach};
use super::evaluate::{OnUnwind, Presets, Value};
use super::find::Loader;
use super::object::Object;
use super::references::References;
use super::syntax::{
    self, each_node, export_name, global_root, identifier_name, key_name, member_name, member_of,
    plain_value, reads_uninitialized, required_source, root_name, root_names, runs_unseen_code,
    target_write, target_writes,
};
use super::yaml;
use crate::paths::basename;
use crate::text::is_js_whitespace;

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
    pub resolved: RefCell<HashMap<(usize, usize), Vec<Answer>>>,
    pub resolving: RefCell<HashSet<(usize, usize)>>,
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
    pub data: Option<Value>,
    pub binds_require: bool,
    pub imports_files: bool,
    pub mutated: Cell<bool>,
    pub(super) runs: RefCell<Vec<(&'static str, Loader)>>,
    pub(super) writes_loaded: Cell<bool>,
    pub(super) reach: Cell<Option<Reach>>,
    pub(super) children: OnceCell<Option<Vec<Weak<Module>>>>,
    pub(super) reachable: Cell<Option<usize>>,
    pub bindings: RefCell<HashMap<String, Rc<Binding>>>,
    pub exports: RefCell<IndexMap<String, Rc<Binding>>>,
    pub namespaces: RefCell<HashMap<(usize, usize), Vec<Answer>>>,
}

impl Module {
    // A module that imports keys its values by the reader's chain length: a value cut at the import cap, or holding lazies that captured a long chain, is wrong for a reader nearer the top.
    pub fn depth_key(&self, chain: &[Rc<str>]) -> usize {
        if self.imports_files { chain.len() } else { 0 }
    }
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
    pub depth: Depth,
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

pub(super) const MUTATING_OBJECT_METHODS: [&str; 4] = [
    "assign",
    "defineProperty",
    "defineProperties",
    "setPrototypeOf",
];

fn receiver(callee: &Expression<'static>, helped: bool) -> (Vec<&'static str>, bool) {
    let Some(callee) = member_of(callee) else {
        return (Vec::new(), false);
    };

    let root = root_name(callee.object());
    let method = member_name(callee).unwrap_or("");
    let global = root == Some("Reflect")
        || (root == Some("Object") && MUTATING_OBJECT_METHODS.contains(&method));

    let roots = if helped {
        Vec::new()
    } else {
        root_names(callee.object())
    };

    (roots, global)
}

fn export_target(target: &AssignmentTarget<'static>) -> Option<&'static str> {
    let member = target.as_member_expression()?;
    match identifier_name(member.object())? {
        "module" if member_name(member) == Some("exports") => Some("default"),
        "exports" => member_name(member),
        _ => None,
    }
}

fn in_packages(file: &str) -> bool {
    file.split('/').any(|part| part == "node_modules")
}

type ExportAssignments = IndexMap<&'static str, Vec<&'static AssignmentExpression<'static>>>;

impl Module {
    pub fn required<'a>(&self, call: &CallExpression<'a>) -> Option<&'a str> {
        if self.binds_require {
            return None;
        }

        required_source(call)
    }

    fn required_call(&self, expression: &Expression<'static>) -> Option<&'static str> {
        match expression {
            Expression::CallExpression(call) => self.required(call),
            _ => None,
        }
    }

    fn collect_bindings(&self, program: &'static Program<'static>) -> ExportAssignments {
        let mut assignments = ExportAssignments::new();
        let mut bindings = self.bindings.borrow_mut();
        let mut exports = self.exports.borrow_mut();
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
                        .and_then(|init| self.required_call(init))
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

        assignments
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

pub(super) fn script_name(file: &str) -> bool {
    [".js", ".ts", ".cjs", ".cts", ".mjs", ".mts"]
        .iter()
        .any(|extension| file.ends_with(extension))
}

#[derive(Clone, Copy)]
enum DataFormat {
    Yaml,
    JsonOrYaml,
}

fn data_format(file: &str) -> Option<DataFormat> {
    if file.ends_with(".yaml") || file.ends_with(".yml") {
        Some(DataFormat::Yaml)
    } else if basename(file) == ".eslintrc" {
        Some(DataFormat::JsonOrYaml)
    } else {
        None
    }
}

fn data_value(bytes: &[u8], format: DataFormat) -> Value {
    let bytes = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(bytes);
    let value = match format {
        DataFormat::Yaml => yaml_value(bytes),
        DataFormat::JsonOrYaml => match std::str::from_utf8(bytes) {
            Ok(text) => {
                let stripped = without_json_comments(text);
                yaml::json(&stripped)
                    .or_else(|| yaml::parse(stripped.as_bytes()))
                    .map_or(Value::Unknown, |tree| tree_value(&tree))
            }
            Err(_) => Value::Unknown,
        },
    };

    // ESLint loads a legacy config as `parsed || {}`.
    match value {
        Value::Null | Value::Bool(false) | Value::Number(0.0) => {
            Value::Object(Object::plain(Vec::new()))
        }
        Value::String(text) if text.is_empty() => Value::Object(Object::plain(Vec::new())),
        Value::Object(_) => value,
        _ => Value::Unknown,
    }
}

fn yaml_value(bytes: &[u8]) -> Value {
    yaml::parse(bytes).map_or(Value::Unknown, |tree| tree_value(&tree))
}

fn tree_value(tree: &yaml::Tree) -> Value {
    match tree {
        yaml::Tree::Null => Value::Null,
        yaml::Tree::Bool(value) => Value::Bool(*value),
        yaml::Tree::Number(number) => Value::Number(*number),
        yaml::Tree::String(text) => Value::String(text.as_str().into()),
        yaml::Tree::Array(items) => Value::Array(Rc::new(items.iter().map(tree_value).collect())),
        yaml::Tree::Object(fields) => Value::Object(Object::plain(
            fields
                .iter()
                .map(|(key, value)| (key.as_str(), tree_value(value)))
                .collect(),
        )),
    }
}

// ESLint runs strip-json-comments 3 over an extensionless .eslintrc before js-yaml reads it, so a `/*` inside a single quoted glob is a comment there.
fn without_json_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut stripped = String::with_capacity(text.len());
    let mut quoted = false;
    let mut copied = 0;
    let mut at = 0;
    while at < bytes.len() {
        let comment_end = match (bytes[at], bytes.get(at + 1)) {
            (b'"', _) => {
                let backslashes = bytes[..at]
                    .iter()
                    .rev()
                    .take_while(|&&byte| byte == b'\\')
                    .count();

                quoted ^= backslashes % 2 == 0;
                None
            }
            _ if quoted => None,
            (b'/', Some(b'/')) => Some(line_comment_end(bytes, at)),
            (b'/', Some(b'*')) => Some(
                bytes[at + 2..]
                    .windows(2)
                    .position(|pair| pair == b"*/")
                    .map_or(bytes.len(), |close| at + close + 4),
            ),
            _ => None,
        };

        let Some(end) = comment_end else {
            at += 1;
            continue;
        };

        stripped.push_str(&text[copied..at]);
        for char in text[at..end].chars() {
            if is_js_whitespace(char) {
                stripped.push(char);
            } else {
                stripped.extend(std::iter::repeat_n(' ', char.len_utf16()));
            }
        }

        copied = end;
        at = end;
    }

    stripped.push_str(&text[copied..]);
    stripped
}

fn line_comment_end(text: &[u8], start: usize) -> usize {
    match text[start..].iter().position(|&byte| byte == b'\n') {
        Some(newline) if text[start + newline - 1] == b'\r' => start + newline - 1,
        Some(newline) => start + newline,
        None => text.len(),
    }
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
        let assignments = module.collect_bindings(program);

        if !in_packages(&module.file) {
            let effects = effects::effects(program);
            module.writes_loaded.set(effects.writes);
            *module.runs.borrow_mut() = effects.runs;
        }

        self.mark_mutations(module, program, &assignments);
    }

    fn mark_mutations(
        &self,
        module: &Module,
        program: &'static Program<'static>,
        assignments: &ExportAssignments,
    ) {
        let permitted: HashSet<_> = assignments
            .values()
            .filter(|items| items.len() == 1)
            .flatten()
            .map(|assignment| assignment.unstable_address())
            .collect();

        let mut references = References::default();
        let mut rebound = Vec::new();
        let mut unscoped = Vec::new();
        let mut mutated = false;
        let mut bound = HashSet::new();
        let mut global_writes = Vec::new();
        let mut globals_untrusted = false;
        let mut require_values = 0;
        let mut require_calls = 0;
        each_node(program, |kind| {
            if let AstKind::BindingIdentifier(binding) = kind {
                bound.insert(binding.name.as_str());
                return;
            }

            match kind {
                AstKind::TSEnumMember(member) => {
                    bound.insert(member.id.static_name().as_str());
                }
                AstKind::WithStatement(_) => globals_untrusted = true,
                AstKind::IdentifierReference(reference) if reference.name == "require" => {
                    require_values += 1;
                }
                AstKind::CallExpression(call)
                    if identifier_name(&call.callee) == Some("require") =>
                {
                    require_calls += 1;
                }
                _ => {}
            }

            globals_untrusted |= runs_unseen_code(kind);

            if let AstKind::AssignmentExpression(assignment) = kind
                && (assignment.operator == AssignmentOperator::Assign
                    || assignment.operator.is_logical())
                && let Some(root) = global_root(&assignment.left)
            {
                global_writes.push((root, &assignment.right));
                return;
            }

            let target = match kind {
                AstKind::AssignmentExpression(assignment)
                    if !permitted.contains(&assignment.unstable_address()) =>
                {
                    Some(&assignment.left)
                }
                AstKind::ForInStatement(statement) => statement.left.as_assignment_target(),
                AstKind::ForOfStatement(statement) => statement.left.as_assignment_target(),
                _ => None,
            };

            if let Some(target) = target {
                target_writes(target, &mut |root, member| {
                    mutated |= member;
                    if member {
                        references.dirty.extend(root);
                    } else {
                        bound.extend(root);
                        rebound.extend(root);
                    }
                });
                return;
            }

            let (root, member) = match kind {
                AstKind::UpdateExpression(update) => {
                    (Vec::from_iter(target_write(&update.argument).0), true)
                }
                AstKind::UnaryExpression(unary) if unary.operator == UnaryOperator::Delete => (
                    Vec::from_iter(root_name(&unary.argument)),
                    unary.argument.is_member_expression(),
                ),
                AstKind::CallExpression(call) => {
                    let helped = self.helper(&call.callee, module).is_some();
                    if !helped && module.required(call).is_none_or(str::is_empty) {
                        references.scan(call.unstable_address(), &call.arguments);
                    }

                    receiver(&call.callee, helped)
                }
                AstKind::TaggedTemplateExpression(tagged) => {
                    let helped = self.helper(&tagged.tag, module).is_some();
                    if !helped {
                        references.scan_template(tagged.unstable_address(), &tagged.quasi);
                    }

                    receiver(&tagged.tag, helped)
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
            unscoped.extend(root);
        });

        globals_untrusted |= require_values > require_calls;
        mutated |= reads_uninitialized(program)
            || global_writes.into_iter().any(|(root, value)| {
                globals_untrusted || bound.contains(root) || !plain_value(value)
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

        aliases::spread(
            program,
            Calls {
                helped: &|callee| self.helper(callee, module).is_some(),
                required: &|call| {
                    module
                        .required(call)
                        .is_some_and(|source| !source.is_empty())
                },
            },
            &mut dirty,
        );
        dirty.extend(rebound);
        dirty.extend(unscoped);

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

        let data = data_format(&file);
        let read = match data {
            Some(_) => yaml::read(&file),
            None => fs::read(&file).ok(),
        };

        let Some(bytes) = read else {
            self.modules.borrow_mut().insert(file, None);
            return None;
        };

        #[cfg(test)]
        PARSES.lock().unwrap().push(file.clone());

        let text = String::from_utf8_lossy(&bytes).into_owned();
        let script = script_name(&file);
        let (program, data) = match data {
            Some(format) => (None, Some(data_value(&bytes, format))),
            None if script => (syntax::parse(&file, &text), None),
            None => (
                syntax::parse(&format!("{file}.js"), &format!("({text}\n)")),
                None,
            ),
        };

        let module = Rc::new(Module {
            file: file.as_str().into(),
            text,
            program,
            data,
            binds_require: program.is_some_and(syntax::binds_require),
            imports_files: program.is_some_and(|program| {
                syntax::imports_files(program, |source| self.presets.known(source).is_none())
            }),
            mutated: Cell::new(false),
            runs: RefCell::default(),
            writes_loaded: Cell::new(false),
            reach: Cell::new(None),
            children: OnceCell::new(),
            reachable: Cell::new(None),
            bindings: RefCell::default(),
            exports: RefCell::default(),
            namespaces: RefCell::default(),
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
