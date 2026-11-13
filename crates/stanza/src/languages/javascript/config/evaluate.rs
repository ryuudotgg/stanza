use std::rc::Rc;

use oxc_ast::ast::{
    Argument, ArrayExpression, ArrayExpressionElement, CallExpression, Expression,
    MemberExpression, ObjectExpression, ObjectPropertyKind,
};

use super::find::{Loader, resolve_module};
use super::module::{Binding, Helper, Module, Session};
use super::object::{Builder, Object};
use super::syntax::{Operand, key_name, member_name, required_source};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ByteOffset(pub u32);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    pub file: Rc<str>,
    pub start: ByteOffset,
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

    fn object_value(
        &self,
        node: &'static ObjectExpression<'static>,
        module: &Rc<Module>,
        chain: &Chain,
    ) -> Value {
        let mut object = Builder::default();
        for item in &node.properties {
            let item = match item {
                ObjectPropertyKind::SpreadProperty(spread) => {
                    let value = self.evaluate(&spread.argument, module, chain);
                    if matches!(value, Value::NoSetting) {
                        continue;
                    }

                    let Some(source) = spread_source(&value) else {
                        return Value::Unknown;
                    };

                    object.spread(source);
                    continue;
                }
                ObjectPropertyKind::ObjectProperty(item) => item,
            };

            let key = (!item.computed).then(|| key_name(&item.key)).flatten();
            let Some(key) = key.filter(|key| !key.is_empty()) else {
                return Value::Unknown;
            };

            object.lazy(key, item, module, chain);
        }

        Value::Object(object.finish())
    }

    fn array_value(
        &self,
        node: &'static ArrayExpression<'static>,
        module: &Rc<Module>,
        chain: &Chain,
    ) -> Value {
        if node.elements.len() > ARRAY_CAP {
            return Value::Unknown;
        }

        let mut result: Vec<Value> = Vec::new();
        for element in &node.elements {
            let value = match element {
                ArrayExpressionElement::Elision(_) => Value::Unknown,
                ArrayExpressionElement::SpreadElement(spread) => {
                    match self.evaluate(&spread.argument, module, chain) {
                        Value::NoSetting => continue,
                        Value::Array(values) => {
                            if result.len() + values.len() > ARRAY_CAP {
                                return Value::Unknown;
                            }

                            result.extend(values.iter().cloned());
                            continue;
                        }
                        _ => return Value::Unknown,
                    }
                }
                _ => element
                    .as_expression()
                    .map_or(Value::Unknown, |item| self.evaluate(item, module, chain)),
            };

            result.push(value);
            if result.len() > ARRAY_CAP {
                return Value::Unknown;
            }
        }

        Value::Array(Rc::new(result))
    }

    fn call_value(
        &self,
        node: &'static CallExpression<'static>,
        module: &Rc<Module>,
        chain: &Chain,
    ) -> Value {
        if let Some(name) = required_source(node) {
            if let Some(preset) = self.presets.known(name) {
                return preset;
            }

            return resolve_module(name, &module.file, Loader::Require)
                .map_or(Value::Unknown, |file| {
                    self.exported(&file, "default", chain)
                });
        }

        match self.helper(&node.callee, module) {
            Some(Helper::Ignore) => Value::Object(Object::plain(vec![])),
            Some(Helper::Identity) => match node.arguments.as_slice() {
                [argument] => argument.as_expression().map_or(Value::Unknown, |argument| {
                    self.evaluate(argument, module, chain)
                }),
                _ => Value::Unknown,
            },
            Some(Helper::Flatten) => {
                let mut result = Vec::new();
                for item in &node.arguments {
                    let target = match item {
                        Argument::SpreadElement(spread) => &spread.argument,
                        _ => item.to_expression(),
                    };

                    match self.evaluate(target, module, chain) {
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

    fn reference_value(&self, name: &str, module: &Rc<Module>, chain: &Chain) -> Value {
        let binding = module.bindings.borrow().get(name).cloned();
        self.binding_value(binding.as_ref(), module, chain)
    }

    fn member_value(
        &self,
        member: &'static MemberExpression<'static>,
        module: &Rc<Module>,
        chain: &Chain,
    ) -> Value {
        let value = self.evaluate(member.object(), module, chain);
        if matches!(value, Value::NoSetting) {
            return value;
        }

        member_name(member).map_or(Value::Unknown, |key| property(self, &value, key))
    }

    pub fn operand_value(&self, operand: Operand, module: &Rc<Module>, chain: &Chain) -> Value {
        match operand {
            Operand::Expression(expression) => self.evaluate(expression, module, chain),
            Operand::Member(member) => self.member_value(member, module, chain),
            Operand::Reference(name) => self.reference_value(name, module, chain),
            Operand::Opaque => Value::Unknown,
        }
    }

    pub fn evaluate(
        &self,
        node: &'static Expression<'static>,
        module: &Rc<Module>,
        chain: &Chain,
    ) -> Value {
        match node {
            Expression::StringLiteral(literal) => Value::String(literal.value.as_str().into()),
            Expression::BooleanLiteral(literal) => Value::Bool(literal.value),
            Expression::NumericLiteral(literal) => Value::Number(literal.value),
            Expression::NullLiteral(_) => Value::Null,
            Expression::TemplateLiteral(template) => {
                if !template.expressions.is_empty() {
                    return Value::Unknown;
                }

                match template.quasis.first() {
                    None => Value::Undefined,
                    Some(quasi) => quasi
                        .value
                        .cooked
                        .map_or(Value::Null, |cooked| Value::String(cooked.as_str().into())),
                }
            }
            Expression::ObjectExpression(object) => self.object_value(object, module, chain),
            Expression::ArrayExpression(array) => self.array_value(array, module, chain),
            Expression::Identifier(reference) => {
                self.reference_value(reference.name.as_str(), module, chain)
            }
            Expression::CallExpression(call) => self.call_value(call, module, chain),
            Expression::TSAsExpression(wrapper) => {
                self.evaluate(&wrapper.expression, module, chain)
            }
            Expression::TSSatisfiesExpression(wrapper) => {
                self.evaluate(&wrapper.expression, module, chain)
            }
            Expression::TSNonNullExpression(wrapper) => {
                self.evaluate(&wrapper.expression, module, chain)
            }
            Expression::TSTypeAssertion(wrapper) => {
                self.evaluate(&wrapper.expression, module, chain)
            }
            Expression::ParenthesizedExpression(wrapper) => {
                self.evaluate(&wrapper.expression, module, chain)
            }
            _ => node
                .as_member_expression()
                .map_or(Value::Unknown, |member| {
                    self.member_value(member, module, chain)
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{Scratch, dir_with};
    use super::*;

    fn exported(root: &Scratch, session: &Session, file: &str, name: &str) -> Value {
        session.exported(&root.join(file), name, &Rc::default())
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

            let root = dir_with(&[
                ("strict.js", r#"export default { curly: "error" };"#),
                ("eslint.config.js", &script),
            ]);

            let session = Session::default();
            for name in ["config", "clean", "*"] {
                let value = exported(&root, &session, "eslint.config.js", name);
                assert!(unknown(&value), "{mutation} leaves {name} {value:?}");
            }
        }
    }

    #[test]
    fn spreads_keep_the_order_values_and_origins_of_copied_keys() {
        let script = "const a = { x: 1, y: 2, \"1\": 7 };\nconst b = { y: 3, z: 4 };\nconst d = { ...a, ...a };\nexport default { w: 0, z: 9, ...a, x: 5, ...b, ...d };\n";
        let root = dir_with(&[("eslint.config.js", script)]);
        let session = Session::default();
        let config = exported(&root, &session, "eslint.config.js", "default");
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
        let start = |text: &str| ByteOffset(u32::try_from(script.find(text).unwrap()).unwrap());
        let starts: Vec<Option<ByteOffset>> = ["w", "z", "x", "y", "v"]
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
        let root = dir_with(&[("eslint.config.js", &script)]);
        let session = Session::default();
        let config = exported(&root, &session, "eslint.config.js", "default");
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
        let root = dir_with(&[(
            "eslint.config.js",
            r#"const config = { rules: { curly: "off" } }; export { config }; export default [config];"#,
        )]);

        let session = Session::default();
        let config = exported(&root, &session, "eslint.config.js", "config");
        let rules = property(&session, &config, "rules");
        assert!(
            matches!(property(&session, &rules, "curly"), Value::String(level) if &*level == "off")
        );
    }

    #[test]
    fn an_empty_require_still_poisons_its_arguments() {
        let root = dir_with(&[(
            "eslint.config.js",
            r#"const config = { rules: { curly: "off" } }; require("", config); export default config;"#,
        )]);

        let session = Session::default();
        assert!(unknown(&exported(
            &root,
            &session,
            "eslint.config.js",
            "default"
        )));
    }

    #[test]
    fn a_self_reference_reads_unknown() {
        let root = dir_with(&[(
            "eslint.config.js",
            "const a = { rules: a.rules }; export default a;",
        )]);

        let session = Session::default();
        let value = exported(&root, &session, "eslint.config.js", "default");
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
            let root = dir_with(&[("eslint.config.js", &script)]);
            let session = Session::default();
            let value = exported(&root, &session, "eslint.config.js", "default");
            assert!(unknown(&value), "{} is {value:?}", &script[..40]);
        }
    }

    #[test]
    fn a_spread_up_to_the_cap_is_kept() {
        let script = format!(
            "const base = [{}]; export default [...base];",
            objects(10000)
        );
        let root = dir_with(&[("eslint.config.js", &script)]);
        let session = Session::default();
        let value = exported(&root, &session, "eslint.config.js", "default");
        assert!(matches!(value, Value::Array(items) if items.len() == 10000));
    }

    #[test]
    fn literals_read_as_their_javascript_values() {
        let script = "export default { big: 1e400, zero: 0, regex: /x/, bigint: 1n, holes: [, 1], quasi: `t`, empty: null, yes: true, name: 'n' };";
        let root = dir_with(&[("eslint.config.js", script)]);
        let session = Session::default();
        let config = exported(&root, &session, "eslint.config.js", "default");
        let read = |key: &str| format!("{:?}", property(&session, &config, key));

        assert_eq!(read("big"), "Number(inf)");
        assert_eq!(read("zero"), "Number(0.0)");
        assert_eq!(read("regex"), "Unknown");
        assert_eq!(read("bigint"), "Unknown");
        assert_eq!(read("holes"), "Array([Unknown, Number(1.0)])");
        assert_eq!(read("quasi"), "String(\"t\")");
        assert_eq!(read("empty"), "Null");
        assert_eq!(read("yes"), "Bool(true)");
        assert_eq!(read("name"), "String(\"n\")");
    }
}
