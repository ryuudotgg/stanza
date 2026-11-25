use std::rc::Rc;

use oxc_ast::ast::{
    Argument, ArrayExpression, ArrayExpressionElement, CallExpression, Expression,
    LogicalExpression, LogicalOperator, MemberExpression, ObjectExpression, ObjectPropertyKind,
    PropertyKind, UnaryExpression, UnaryOperator,
};

use super::depth::Height;
use super::find::{Loader, resolve_module};
use super::module::{Binding, Helper, Module, Session, script_name};
use super::object::{Builder, Object};
use super::syntax::{key_name, member_name};

const ARRAY_CAP: usize = 10000;
pub const CHAIN_CAP: usize = 32;

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
    Array(Rc<Vec<Value>>),
    Object(Rc<Object>),
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

impl Value {
    fn truthy(&self) -> Option<bool> {
        match self {
            Value::Undefined | Value::Null => Some(false),
            Value::Bool(value) => Some(*value),
            Value::Number(value) => Some(*value != 0.0 && !value.is_nan()),
            Value::String(value) => Some(!value.is_empty()),
            Value::Array(_) | Value::Object(_) => Some(true),
            Value::Unknown => None,
        }
    }
}

pub fn property(session: &Session, value: &Value, key: &str) -> Value {
    match value {
        Value::Undefined => Value::Undefined,
        Value::Object(object) => object.get(session, key),
        _ => Value::Unknown,
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

        preset.cloned()
    }
}

// The nesting guard charges these wrappers and member dots a fraction of a level, so walking them by recursion would spend frames the guard never counted.
fn unwrapped(mut node: &'static Expression<'static>) -> &'static Expression<'static> {
    loop {
        node = match node {
            Expression::TSAsExpression(wrapper) => &wrapper.expression,
            Expression::TSSatisfiesExpression(wrapper) => &wrapper.expression,
            Expression::TSNonNullExpression(wrapper) => &wrapper.expression,
            Expression::TSTypeAssertion(wrapper) => &wrapper.expression,
            Expression::ParenthesizedExpression(wrapper) => &wrapper.expression,
            _ => return node,
        };
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

        let depth = (
            module.depth_key(chain),
            self.depth.scope(module.imports_files),
        );
        if let Some(value) = self
            .depth
            .served(binding.resolved.borrow().get(&depth), chain)
        {
            return value;
        }

        if !binding.resolving.borrow_mut().insert(depth) {
            return Value::Unknown;
        }

        let _unwound = OnUnwind(|| {
            if let Ok(mut resolving) = binding.resolving.try_borrow_mut() {
                resolving.remove(&depth);
            }
        });

        let built = self.depth.link(|| {
            if let Some(imported) = &binding.imported {
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
            }
        });

        binding.resolving.borrow_mut().remove(&depth);
        let Some((value, height, touched)) = built else {
            return Value::Unknown;
        };

        let mut resolved = binding.resolved.borrow_mut();
        self.depth.remember(
            resolved.entry(depth).or_default(),
            chain,
            &value,
            height,
            touched,
        );

        value
    }

    pub fn exported(&self, file: &str, name: &str, chain: &Chain) -> Value {
        let Some(module) = self.module_at(file) else {
            return Value::Unknown;
        };

        self.depth.touch(&module.file);
        if module.program.is_none()
            || module.mutated.get()
            || self.runs_writer(&module, chain.len())
        {
            return Value::Unknown;
        }

        if chain.iter().any(|seen| **seen == *module.file) {
            self.depth.hit();
            return Value::Unknown;
        }

        self.depth.enter_file(chain.len() + 1);
        if chain.len() >= CHAIN_CAP {
            return Value::Unknown;
        }

        if name != "*" {
            let mut next = chain.as_ref().clone();
            next.push(module.file.clone());
            let binding = module.exports.borrow().get(name).cloned();
            return self.binding_value(binding.as_ref(), &module, &Rc::new(next));
        }

        let key = (
            module.depth_key(chain),
            self.depth.scope(module.imports_files),
        );
        if let Some(value) = self
            .depth
            .served(module.namespaces.borrow().get(&key), chain)
        {
            return value;
        }

        let file: Rc<str> = file.into();
        let (namespace, touched) = self.depth.traced(|| {
            Object::namespace(module.exports.borrow().keys(), &file, chain, &self.depth)
        });

        let value = Value::Object(namespace);
        if !self.depth.cut() {
            let mut namespaces = module.namespaces.borrow_mut();
            self.depth.remember(
                namespaces.entry(key).or_default(),
                chain,
                &value,
                Height::default(),
                touched,
            );
        }

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
                    let Some(source) = spread_source(&value) else {
                        return Value::Unknown;
                    };

                    object.spread(source);
                    continue;
                }
                ObjectPropertyKind::ObjectProperty(item) => item,
            };

            let key = (!item.computed).then(|| key_name(&item.key)).flatten();
            let Some(key) = key else {
                return Value::Unknown;
            };

            let sets_prototype = key == "__proto__"
                && item.kind == PropertyKind::Init
                && !item.shorthand
                && !item.method
                && script_name(&module.file);

            if sets_prototype {
                return Value::Unknown;
            }

            object.lazy(key, item, module, chain, &self.depth);
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
        if let Some(name) = module.required(node) {
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
        let mut keys = vec![(member_name(member), member.optional())];
        let mut object = unwrapped(member.object());
        while let Some(inner) = object.as_member_expression() {
            keys.push((member_name(inner), inner.optional()));
            object = unwrapped(inner.object());
        }

        let mut value = self.evaluate(object, module, chain);
        for (key, optional) in keys.into_iter().rev() {
            if optional && matches!(value, Value::Null | Value::Undefined) {
                return Value::Undefined;
            }

            value = key.map_or(Value::Unknown, |key| property(self, &value, key));
        }

        value
    }

    fn logical_value(
        &self,
        node: &'static LogicalExpression<'static>,
        module: &Rc<Module>,
        chain: &Chain,
    ) -> Value {
        if node.operator != LogicalOperator::Or {
            return Value::Unknown;
        }

        let left = self.evaluate(&node.left, module, chain);
        match left.truthy() {
            Some(true) => left,
            Some(false) => self.evaluate(&node.right, module, chain),
            None => Value::Unknown,
        }
    }

    fn negated(
        &self,
        node: &'static UnaryExpression<'static>,
        module: &Rc<Module>,
        chain: &Chain,
    ) -> Value {
        if node.operator != UnaryOperator::UnaryNegation {
            return Value::Unknown;
        }

        match self.evaluate(&node.argument, module, chain) {
            Value::Number(number) => Value::Number(-number),
            _ => Value::Unknown,
        }
    }

    pub fn evaluate(
        &self,
        node: &'static Expression<'static>,
        module: &Rc<Module>,
        chain: &Chain,
    ) -> Value {
        let node = unwrapped(node);
        if let Some(member) = node.as_member_expression() {
            return self.member_value(member, module, chain);
        }

        let Some(_frame) = self.depth.frame() else {
            return Value::Unknown;
        };

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
            Expression::ChainExpression(optional) => optional
                .expression
                .as_member_expression()
                .map_or(Value::Unknown, |member| {
                    self.member_value(member, module, chain)
                }),
            Expression::LogicalExpression(logical) => self.logical_value(logical, module, chain),
            Expression::UnaryExpression(unary) => self.negated(unary, module, chain),
            _ => Value::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::dir_with;
    use super::*;
    use crate::testing::scratch::Scratch;

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
    fn spreads_keep_the_order_and_values_of_copied_keys() {
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
        assert!(unknown(&value));
    }

    #[test]
    fn exports_share_binding_and_namespace_cells() {
        let root = dir_with(&[
            (
                "base.js",
                "const config = { printWidth: 91 }; export { config }; export default config;",
            ),
            (
                "self.js",
                r#"import config from "./self.js"; export default config;"#,
            ),
        ]);

        let session = Session::default();
        let object = |value: Value| match value {
            Value::Object(object) => object,
            other => panic!("an object export evaluates to an object: {other:?}"),
        };

        let config = object(exported(&root, &session, "base.js", "config"));
        for name in ["config", "default", "default", "config"] {
            let again = object(exported(&root, &session, "base.js", name));
            assert!(Rc::ptr_eq(&config, &again), "{name} is not the config cell");
        }

        let namespace = object(exported(&root, &session, "base.js", "*"));
        let again = object(exported(&root, &session, "base.js", "*"));
        assert!(Rc::ptr_eq(&namespace, &again));

        let namespace = Value::Object(namespace);
        for name in ["config", "default"] {
            let read = object(property(&session, &namespace, name));
            assert!(Rc::ptr_eq(&config, &read), "namespace {name} is a copy");
        }

        assert!(unknown(&exported(&root, &session, "self.js", "default")));
    }

    #[test]
    fn an_initializer_reading_an_uninitialized_binding_makes_the_module_unknown() {
        for script in [
            "const o = { a: o, x: 1 }; export default o;",
            "const o = { a: o }; export default { x: 1, extra: o };",
            "let o = { a: { b: [o] } }; export default { x: 1 };",
            "export const { o } = { o: o }; export default { x: 1 };",
            "const a = { b }; const b = { a }; export default { x: 1, a };",
            "const a = 1, b = c, c = 2; export default { x: 1 };",
            "export default { x: 1, c: C }; class C {}",
            "const o = { semi: (() => o)() }; export default { x: 1 };",
            "const o = { a: (function () { return o; })() }; export default { x: 1 };",
            "const o = { a: null || o }; export default { x: 1 };",
            "const o = { a: false ? 1 : o }; export default { x: 1 };",
            "const o = { a: process.env.X || o }; export default { x: 1 };",
            "const o = ((p) => o)(1); export default { x: 1 };",
        ] {
            let root = dir_with(&[("eslint.config.js", script)]);
            let session = Session::default();
            let value = exported(&root, &session, "eslint.config.js", "default");
            assert!(unknown(&value), "{script} is {value:?}");
        }

        for script in [
            "const a = { semi: true }; const o = { a, b: a, x: 1 }; export default o;",
            "const o = { f() { return o; }, g: () => o, x: 1 }; export default o;",
            "var o = { a: o }; export default { x: 1 };",
            "const a = {}, b = { a }; export default { x: 1, b };",
            "const g = () => later; const later = {}; export default { x: 1, g };",
            "const h = f(); function f() {} export default { x: 1 };",
            "const base = { x: 1 }; export default base || fallback; const fallback = {};",
            "const o = { semi: true || o, x: 1 }; export default o;",
            "const o = { a: true ? 1 : o, x: 1 }; export default o;",
            "const o = { a: 1 ?? o, x: 1 }; export default o;",
            "const o = { a: 0 && o, x: 1 }; export default o;",
            "const settings = ((settings) => settings)({}); export default { x: 1 };",
            "const iter = (function* () { yield iter; })(); export default { x: 1 };",
            "const o = (function o() { return o; })(); export default { x: 1 };",
            "const o = (() => { const o = 1; return o; })(); export default { x: 1 };",
        ] {
            let root = dir_with(&[("eslint.config.js", script)]);
            let session = Session::default();
            let value = exported(&root, &session, "eslint.config.js", "default");
            let x = property(&session, &value, "x");
            assert!(matches!(x, Value::Number(1.0)), "{script} reads x as {x:?}");
        }
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

    #[test]
    fn unary_minus_negates_numbers_alone() {
        let script = "const one = 1; export default { neg: -1, zero: -0, twice: - -2, bound: -one, inf: -1e400, text: -'1', flag: -true, none: -null, plus: +1, not: !0, kind: typeof 1, gone: void 0 };";
        let root = dir_with(&[("eslint.config.js", script)]);
        let session = Session::default();
        let config = exported(&root, &session, "eslint.config.js", "default");
        let read = |key: &str| format!("{:?}", property(&session, &config, key));

        assert_eq!(read("neg"), "Number(-1.0)");
        assert_eq!(read("zero"), "Number(-0.0)");
        assert_eq!(read("twice"), "Number(2.0)");
        assert_eq!(read("bound"), "Number(-1.0)");
        assert_eq!(read("inf"), "Number(-inf)");
        for key in ["text", "flag", "none", "plus", "not", "kind", "gone"] {
            assert_eq!(read(key), "Unknown", "{key}");
        }
    }

    #[test]
    fn an_empty_key_is_an_own_property() {
        let script = "export default { '': { semi: 1 }, \"\": 2, x: 3 };";
        let root = dir_with(&[("eslint.config.js", script)]);
        let session = Session::default();
        let config = exported(&root, &session, "eslint.config.js", "default");
        let Value::Object(object) = &config else {
            panic!("an object literal evaluates to an object: {config:?}");
        };

        let read = |key: &str| format!("{:?}", property(&session, &config, key));
        assert_eq!(object.keys(), ["", "x"]);
        assert_eq!(read(""), "Number(2.0)");
        assert_eq!(read("semi"), "Undefined");
        assert_eq!(read("x"), "Number(3.0)");
    }
}
