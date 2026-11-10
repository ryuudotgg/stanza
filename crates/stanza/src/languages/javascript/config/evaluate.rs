use std::rc::Rc;

use super::estree::{Node, field, flag, items, kind, start, text};
use super::find::{Loader, resolve_module};
use super::module::{Binding, Helper, Module, Session, member_name, name_of, required_source};
use super::object::{Builder, Object};

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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    pub file: Rc<str>,
    pub start: u64,
}

// Left marked as resolving after a panic, a lazy or binding reads as Unknown, so the next file reports an unread config instead of the panic.
pub struct OnUnwind<F: FnMut()>(pub F);

impl<F: FnMut()> Drop for OnUnwind<F> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            (self.0)();
        }
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
        Value::Object(object) => object.origin(key),
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
        let _unwound = OnUnwind(|| binding.resolved.set(false));

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
            Value::Object(Object::namespace(
                module.exports.borrow().keys(),
                &file,
                chain,
            ))
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
        let mut object = Builder::default();
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

                object.spread(source);
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

            object.lazy(key, item, module, chain);
        }

        Value::Object(object.finish())
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
    fn spreads_keep_the_order_values_and_origins_of_copied_keys() {
        let script = "const a = { x: 1, y: 2, \"1\": 7 };\nconst b = { y: 3, z: 4 };\nconst d = { ...a, ...a };\nexport default { w: 0, z: 9, ...a, x: 5, ...b, ...d };\n";
        let root = Scratch::with(&[("eslint.config.js", script)]);
        let session = Session::default();
        let config = root.exported(&session, "eslint.config.js", "default");
        let Value::Object(object) = &config else {
            panic!("an object literal evaluates to an object: {config:?}");
        };

        assert_eq!(object.keys(), ["1", "w", "z", "x", "y"]);
        let numbers: Vec<Option<f64>> = ["1", "w", "z", "x", "y", "v"]
            .iter()
            .map(|key| match property(&session, &config, key) {
                Value::Number(number) => Some(number),
                _ => None,
            })
            .collect();

        assert_eq!(
            numbers,
            [Some(7.0), Some(0.0), Some(4.0), Some(1.0), Some(2.0), None]
        );
        let start = |text: &str| u64::try_from(script.find(text).unwrap()).unwrap();
        let starts: Vec<Option<u64>> = ["w", "z", "x", "y", "v"]
            .iter()
            .map(|key| origin(&config, key).map(|origin| origin.start))
            .collect();

        assert_eq!(
            starts,
            [
                Some(start("w: 0")),
                Some(start("z: 4")),
                Some(start("x: 1")),
                Some(start("y: 2")),
                None
            ]
        );
    }

    #[test]
    fn a_source_spread_twice_per_level_is_read_once_per_level() {
        let levels: String = (1..=60)
            .map(|level| {
                format!(
                    "const x{level} = {{ ...x{}, ...x{} }};\n",
                    level - 1,
                    level - 1
                )
            })
            .collect();

        let script = format!("const x0 = {{ k: 1 }};\n{levels}export default x60;\n");
        let root = Scratch::with(&[("eslint.config.js", &script)]);
        let session = Session::default();
        let config = root.exported(&session, "eslint.config.js", "default");
        let Value::Object(object) = &config else {
            panic!("an object literal evaluates to an object: {config:?}");
        };

        assert_eq!(object.keys(), ["k"]);
        assert!(matches!(property(&session, &config, "k"), Value::Number(number) if number == 1.0));
        assert!(matches!(
            property(&session, &config, "missing"),
            Value::Undefined
        ));
        assert!(origin(&config, "missing").is_none());
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
