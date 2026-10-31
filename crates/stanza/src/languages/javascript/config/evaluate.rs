use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexMap;

use super::estree::{Node, field, flag, items, kind, start, text};
use super::find::{Loader, resolve_module};
use super::module::{Binding, Helper, Module, Session, member_name, name_of, required_source};

const ARRAY_CAP: usize = 10000;
const CHAIN_CAP: usize = 32;

pub type Chain = Rc<Vec<Rc<str>>>;

#[derive(Clone, Debug, Default)]
pub enum Value {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(Rc<str>),
    Unknown,
    NoSetting,
    Array(Rc<Vec<Value>>),
    Object(Rc<Object>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub file: Rc<str>,
    pub start: u64,
}

#[derive(Debug, Default)]
pub struct Object {
    slots: IndexMap<String, Slot>,
    origins: IndexMap<String, Origin>,
    every_key_empty_array: bool,
}

#[derive(Debug)]
enum Slot {
    Plain(Value),
    Lazy(Lazy),
    Spread(Rc<Object>),
    Namespace { file: Rc<str>, chain: Chain },
}

struct Lazy {
    item: &'static Node,
    module: Rc<Module>,
    chain: Chain,
    state: RefCell<LazyState>,
}

impl std::fmt::Debug for Lazy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Lazy")
    }
}

#[derive(Debug)]
enum LazyState {
    Pending,
    Resolving,
    Done(Value),
}

fn array_index(key: &str) -> Option<u32> {
    let index: u32 = key.parse().ok()?;
    (index != u32::MAX && index.to_string() == key).then_some(index)
}

impl Object {
    pub fn plain(entries: Vec<(&str, Value)>) -> Rc<Self> {
        Rc::new(Self {
            slots: entries
                .into_iter()
                .map(|(key, value)| (key.to_owned(), Slot::Plain(value)))
                .collect(),
            ..Self::default()
        })
    }

    pub fn every_key_empty_array() -> Rc<Self> {
        Rc::new(Self {
            every_key_empty_array: true,
            ..Self::default()
        })
    }

    pub fn keys(&self) -> Vec<String> {
        let mut indexes: Vec<(u32, &String)> = Vec::new();
        let mut names: Vec<&String> = Vec::new();
        for key in self.slots.keys() {
            match array_index(key) {
                Some(index) => indexes.push((index, key)),
                None => names.push(key),
            }
        }

        indexes.sort_unstable_by_key(|(index, _)| *index);
        indexes
            .into_iter()
            .map(|(_, key)| key)
            .chain(names)
            .cloned()
            .collect()
    }

    pub fn get(&self, session: &Session, key: &str) -> Value {
        if self.every_key_empty_array {
            return Value::Array(Rc::default());
        }

        match self.slots.get(key) {
            None => Value::Undefined,
            Some(Slot::Plain(value)) => value.clone(),
            Some(Slot::Spread(source)) => source.get(session, key),
            Some(Slot::Namespace { file, chain }) => session.exported(file, key, chain),
            Some(Slot::Lazy(lazy)) => lazy.force(session),
        }
    }

    pub fn origin(&self, key: &str) -> Option<&Origin> {
        self.origins.get(key)
    }
}

impl Lazy {
    fn force(&self, session: &Session) -> Value {
        match &*self.state.borrow() {
            LazyState::Pending => {}
            LazyState::Resolving => return Value::Unknown,
            LazyState::Done(value) => return value.clone(),
        }

        *self.state.borrow_mut() = LazyState::Resolving;

        let value = if flag(self.item, "method") || text(self.item, "kind") != Some("init") {
            Value::Unknown
        } else {
            field(self.item, "value").map_or(Value::Unknown, |node| {
                session.evaluate(node, &self.module, &self.chain)
            })
        };

        *self.state.borrow_mut() = LazyState::Done(value.clone());
        value
    }
}

pub fn property(session: &Session, value: &Value, key: &str) -> Value {
    match value {
        Value::Undefined | Value::NoSetting => value.clone(),
        Value::Object(object) => object.get(session, key),
        _ => Value::Unknown,
    }
}

pub fn origin(value: &Value, key: &str) -> Option<Origin> {
    match value {
        Value::Object(object) => object.origin(key).cloned(),
        _ => None,
    }
}

pub struct Presets {
    recommended: Value,
    all: Value,
    js: Value,
    typescript: Value,
    prettier: Value,
    config: Value,
}

fn curly(level: &str) -> Value {
    Value::Object(Object::plain(vec![(
        "rules",
        Value::Object(Object::plain(vec![("curly", Value::String(level.into()))])),
    )]))
}

impl Default for Presets {
    fn default() -> Self {
        let recommended_rules =
            Object::plain(vec![("rules", Value::Object(Object::plain(vec![])))]);
        let configs = Object::plain(vec![
            ("recommended", Value::Object(recommended_rules)),
            ("all", curly("error")),
        ]);

        Self {
            recommended: Value::Object(Object::plain(vec![])),
            all: curly("error"),
            js: Value::Object(Object::plain(vec![("configs", Value::Object(configs))])),
            typescript: Value::Object(Object::plain(vec![(
                "configs",
                Value::Object(Object::every_key_empty_array()),
            )])),
            prettier: curly("off"),
            config: Value::Object(Object::plain(vec![])),
        }
    }
}

const PRETTIER: [&str; 5] = [
    "prettier",
    "eslint-config-prettier",
    "eslint-config-prettier/flat",
    "plugin:prettier/recommended",
    "eslint-plugin-prettier/recommended",
];

fn plugin_package(name: &str) -> bool {
    if name.starts_with("eslint-plugin-") {
        return true;
    }

    let Some((scope, package)) = name.strip_prefix('@').and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };

    !scope.is_empty()
        && package.strip_prefix("eslint-plugin").is_some_and(|rest| {
            rest.is_empty()
                || (rest.starts_with('-') && !rest.contains(['\n', '\r', '\u{2028}', '\u{2029}']))
        })
}

impl Presets {
    pub fn known(&self, name: &str) -> Option<Value> {
        let preset = match name {
            "eslint:recommended" => Some(&self.recommended),
            "eslint:all" => Some(&self.all),
            "@eslint/js" => Some(&self.js),
            "typescript-eslint" => Some(&self.typescript),
            "eslint/config" => Some(&self.config),
            _ if PRETTIER.contains(&name) => Some(&self.prettier),
            _ => None,
        };

        if let Some(preset) = preset {
            return Some(preset.clone());
        }

        let parts: Vec<&str> = name.split('/').collect();
        let count = if name.starts_with('@') { 2 } else { 1 };
        let package = parts[..count.min(parts.len())].join("/");
        (name.starts_with("plugin:") || plugin_package(&package)).then_some(Value::NoSetting)
    }
}

fn spread_source(value: &Value) -> Option<&Rc<Object>> {
    match value {
        Value::Object(object) => Some(object),
        _ => None,
    }
}

impl Session {
    pub fn binding_value(
        &self,
        binding: Option<&Rc<Binding>>,
        module: &Rc<Module>,
        chain: &Chain,
    ) -> Value {
        let Some(binding) = binding else {
            return Value::Unknown;
        };

        if binding.mutable.get() {
            return Value::Unknown;
        }

        if binding.resolved.get() {
            return binding.value.borrow().clone();
        }

        binding.resolved.set(true);
        *binding.value.borrow_mut() = Value::Unknown;

        let value = if let Some(imported) = &binding.imported {
            match self.presets.known(&imported.source) {
                Some(preset) if imported.name == "default" || imported.name == "*" => preset,
                Some(preset) => property(self, &preset, &imported.name),
                None => resolve_module(&imported.source, &module.file, imported.loader)
                    .map_or(Value::Unknown, |file| {
                        self.exported(&file, &imported.name, chain)
                    }),
            }
        } else if let Some(local) = &binding.local {
            let target = module.bindings.borrow().get(local).cloned();
            self.binding_value(target.as_ref(), module, chain)
        } else {
            binding
                .node
                .map_or(Value::Unknown, |node| self.evaluate(node, module, chain))
        };

        *binding.value.borrow_mut() = value.clone();
        value
    }

    pub fn exported(&self, file: &str, name: &str, chain: &Chain) -> Value {
        let Some(module) = self.module_at(file) else {
            return Value::Unknown;
        };

        if module.program.is_none()
            || module.mutated.get()
            || chain.iter().any(|seen| **seen == *module.file)
            || chain.len() >= CHAIN_CAP
        {
            return Value::Unknown;
        }

        if let Some(value) = module.values.borrow().get(name) {
            return value.clone();
        }

        let mut next = chain.as_ref().clone();
        next.push(module.file.clone());
        let next = Rc::new(next);

        let value = if name == "*" {
            let file: Rc<str> = file.into();
            let slots = module
                .exports
                .borrow()
                .keys()
                .map(|key| {
                    let slot = Slot::Namespace {
                        file: file.clone(),
                        chain: chain.clone(),
                    };

                    (key.clone(), slot)
                })
                .collect();

            Value::Object(Rc::new(Object {
                slots,
                ..Object::default()
            }))
        } else {
            let binding = module.exports.borrow().get(name).cloned();
            self.binding_value(binding.as_ref(), &module, &next)
        };

        module
            .values
            .borrow_mut()
            .insert(name.to_owned(), value.clone());
        value
    }

    fn object_value(&self, node: &'static Node, module: &Rc<Module>, chain: &Chain) -> Value {
        let mut result = Object::default();
        for item in items(node, "properties") {
            if kind(item) == "SpreadElement" {
                let value = field(item, "argument").map_or(Value::Unknown, |argument| {
                    self.evaluate(argument, module, chain)
                });

                if matches!(value, Value::NoSetting) {
                    continue;
                }

                let Some(source) = spread_source(&value) else {
                    return Value::Unknown;
                };

                for key in source.keys() {
                    result.slots.insert(key, Slot::Spread(source.clone()));
                }

                for (key, location) in &source.origins {
                    result.origins.insert(key.clone(), location.clone());
                }

                continue;
            }

            let key = if flag(item, "computed") {
                None
            } else {
                field(item, "key").and_then(name_of)
            };

            let Some(key) = key.filter(|key| !key.is_empty()) else {
                return Value::Unknown;
            };

            let lazy = Lazy {
                item,
                module: module.clone(),
                chain: chain.clone(),
                state: RefCell::new(LazyState::Pending),
            };

            result.slots.insert(key.to_owned(), Slot::Lazy(lazy));
            result.origins.insert(
                key.to_owned(),
                Origin {
                    file: module.file.clone(),
                    start: start(item),
                },
            );
        }

        Value::Object(Rc::new(result))
    }

    fn array_value(&self, node: &'static Node, module: &Rc<Module>, chain: &Chain) -> Value {
        let elements = items(node, "elements");
        if elements.len() > ARRAY_CAP {
            return Value::Unknown;
        }

        let mut result: Vec<Value> = Vec::new();
        for item in elements {
            let spread = kind(item) == "SpreadElement";
            let value = if item.is_null() {
                Value::Unknown
            } else if spread {
                field(item, "argument").map_or(Value::Unknown, |argument| {
                    self.evaluate(argument, module, chain)
                })
            } else {
                self.evaluate(item, module, chain)
            };

            if spread {
                match value {
                    Value::NoSetting => continue,
                    Value::Array(values) => {
                        if result.len() + values.len() > ARRAY_CAP {
                            return Value::Unknown;
                        }

                        result.extend(values.iter().cloned());
                    }
                    _ => return Value::Unknown,
                }
            } else {
                result.push(value);
            }

            if result.len() > ARRAY_CAP {
                return Value::Unknown;
            }
        }

        Value::Array(Rc::new(result))
    }

    fn call_value(&self, node: &'static Node, module: &Rc<Module>, chain: &Chain) -> Value {
        if let Some(name) = required_source(node) {
            if let Some(preset) = self.presets.known(name) {
                return preset;
            }

            return resolve_module(name, &module.file, Loader::Require)
                .map_or(Value::Unknown, |file| {
                    self.exported(&file, "default", chain)
                });
        }

        let arguments = items(node, "arguments");
        let Some(callee) = field(node, "callee") else {
            return Value::Unknown;
        };

        match self.helper(callee, module) {
            Some(Helper::Ignore) => Value::Object(Object::plain(vec![])),
            Some(Helper::Identity) => match arguments {
                [argument] => self.evaluate(argument, module, chain),
                _ => Value::Unknown,
            },
            Some(Helper::Flatten) => {
                let mut result = Vec::new();
                for item in arguments {
                    let target = if kind(item) == "SpreadElement" {
                        field(item, "argument")
                    } else {
                        Some(item)
                    };

                    match target.map_or(Value::Unknown, |target| {
                        self.evaluate(target, module, chain)
                    }) {
                        Value::Array(values) => result.extend(values.iter().cloned()),
                        value => result.push(value),
                    }
                }

                if result.len() > ARRAY_CAP {
                    Value::Unknown
                } else {
                    Value::Array(Rc::new(result))
                }
            }
            None => Value::Unknown,
        }
    }

    fn literal_value(node: &Node, module: &Module) -> Value {
        if field(node, "regex").is_some() || field(node, "bigint").is_some() {
            return Value::Unknown;
        }

        match node.get("value") {
            Some(serde_json::Value::String(value)) => Value::String(value.as_str().into()),
            Some(serde_json::Value::Bool(value)) => Value::Bool(*value),
            Some(serde_json::Value::Number(_)) if module.infinite.contains(&start(node)) => {
                Value::Number(f64::INFINITY)
            }
            Some(serde_json::Value::Number(value)) => {
                Value::Number(value.as_f64().unwrap_or(f64::NAN))
            }
            _ => Value::Null,
        }
    }

    pub fn evaluate(&self, node: &'static Node, module: &Rc<Module>, chain: &Chain) -> Value {
        match kind(node) {
            "Literal" => Self::literal_value(node, module),
            "TemplateLiteral" => {
                if !items(node, "expressions").is_empty() {
                    return Value::Unknown;
                }

                match items(node, "quasis").first() {
                    None => Value::Undefined,
                    Some(quasi) => match quasi.get("value").and_then(|value| value.get("cooked")) {
                        Some(serde_json::Value::String(cooked)) => {
                            Value::String(cooked.as_str().into())
                        }
                        _ => Value::Null,
                    },
                }
            }
            "ObjectExpression" => self.object_value(node, module, chain),
            "ArrayExpression" => self.array_value(node, module, chain),
            "Identifier" => {
                let binding =
                    text(node, "name").and_then(|name| module.bindings.borrow().get(name).cloned());

                self.binding_value(binding.as_ref(), module, chain)
            }
            "MemberExpression" => {
                let value = field(node, "object").map_or(Value::Unknown, |object| {
                    self.evaluate(object, module, chain)
                });

                if matches!(value, Value::NoSetting) {
                    return value;
                }

                member_name(node).map_or(Value::Unknown, |key| property(self, &value, key))
            }
            "CallExpression" => self.call_value(node, module, chain),
            "TSAsExpression"
            | "TSSatisfiesExpression"
            | "TSNonNullExpression"
            | "TSTypeAssertion"
            | "ParenthesizedExpression" => field(node, "expression")
                .map_or(Value::Unknown, |expression| {
                    self.evaluate(expression, module, chain)
                }),
            _ => Value::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn with(files: &[(&str, &str)]) -> Self {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("stanza-eval-{}-{id}", std::process::id()));
            for (name, text) in files {
                let path = dir.join(name);
                fs::create_dir_all(path.parent().expect("a file has a directory"))
                    .expect("the scratch directory is writable");
                fs::write(path, text).expect("the scratch file is writable");
            }

            Self(dir)
        }

        fn exported(&self, session: &Session, file: &str, name: &str) -> Value {
            let path = self.0.join(file).to_string_lossy().into_owned();
            session.exported(&path, name, &Rc::default())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn unknown(value: &Value) -> bool {
        matches!(value, Value::Unknown)
    }

    fn objects(count: usize) -> String {
        vec!["{}"; count].join(",")
    }

    #[test]
    fn member_mutations_invalidate_all_module_bindings_and_exports() {
        let mutations = [
            r#"const { rules } = config; rules.curly = "error";"#,
            r#"for (const item of [config]) item.rules.curly = "error";"#,
            r#"function change(item) { item.rules.curly = "error"; }"#,
            r#"const list = [config]; list[0].rules.curly = "error";"#,
            "config.rules.curly++;",
            "delete config.rules.curly;",
            "Object.assign(config.rules, strict);",
            r#"Object.defineProperty(config.rules, "curly", { value: "error" });"#,
            r#"Object.defineProperties(config.rules, { curly: { value: "error" } });"#,
            "Object.setPrototypeOf(config.rules, strict);",
            r#"Reflect.set(config.rules, "curly", "error");"#,
            r#"Reflect.get(config.rules, "curly");"#,
        ];

        for mutation in mutations {
            let script = format!(
                r#"import strict from "./strict.js";
                const config = {{ rules: {{ curly: "off" }} }};
                const clean = {{ rules: {{ curly: "off" }} }};
                {mutation}
                export {{ config, clean }}; export default clean;"#
            );

            let root = Scratch::with(&[
                ("strict.js", r#"export default { curly: "error" };"#),
                ("eslint.config.js", &script),
            ]);

            let session = Session::default();
            for name in ["config", "clean", "*"] {
                let value = root.exported(&session, "eslint.config.js", name);
                assert!(unknown(&value), "{mutation} leaves {name} {value:?}");
            }
        }
    }

    #[test]
    fn an_unmutated_module_exports_its_values() {
        let root = Scratch::with(&[(
            "eslint.config.js",
            r#"const config = { rules: { curly: "off" } }; export { config }; export default [config];"#,
        )]);

        let session = Session::default();
        let config = root.exported(&session, "eslint.config.js", "config");
        let rules = property(&session, &config, "rules");
        assert!(
            matches!(property(&session, &rules, "curly"), Value::String(level) if &*level == "off")
        );
    }

    #[test]
    fn an_empty_require_still_poisons_its_arguments() {
        let root = Scratch::with(&[(
            "eslint.config.js",
            r#"const config = { rules: { curly: "off" } }; require("", config); export default config;"#,
        )]);

        let session = Session::default();
        assert!(unknown(&root.exported(
            &session,
            "eslint.config.js",
            "default"
        )));
    }

    #[test]
    fn a_self_reference_reads_unknown() {
        let root = Scratch::with(&[(
            "eslint.config.js",
            "const a = { rules: a.rules }; export default a;",
        )]);

        let session = Session::default();
        let value = root.exported(&session, "eslint.config.js", "default");
        assert!(unknown(&property(&session, &value, "rules")));
    }

    #[test]
    fn oversized_arrays_are_unknown() {
        let doubling: Vec<String> = (0..24)
            .map(|index| format!("const a{} = [...a{index}, ...a{index}];", index + 1))
            .collect();

        let scripts = [
            format!("export default [{}];", objects(10001)),
            format!(
                "const base = [{}]; export default [...base, {{}}];",
                objects(10000)
            ),
            format!(
                r#"import {{ defineConfig }} from "eslint/config"; const base = [{}]; export default defineConfig(base, {{}});"#,
                objects(10000)
            ),
            format!(
                "const a0 = [{{}}]; {} export default a24;",
                doubling.join("\n")
            ),
        ];

        for script in scripts {
            let root = Scratch::with(&[("eslint.config.js", &script)]);
            let session = Session::default();
            let value = root.exported(&session, "eslint.config.js", "default");
            assert!(unknown(&value), "{} is {value:?}", &script[..40]);
        }
    }

    #[test]
    fn a_spread_up_to_the_cap_is_kept() {
        let script = format!(
            "const base = [{}]; export default [...base];",
            objects(10000)
        );
        let root = Scratch::with(&[("eslint.config.js", &script)]);
        let session = Session::default();
        let value = root.exported(&session, "eslint.config.js", "default");
        assert!(matches!(value, Value::Array(items) if items.len() == 10000));
    }
}
