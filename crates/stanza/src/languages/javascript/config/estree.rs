use std::fmt;
use std::hash::{Hash, Hasher};

use oxc_allocator::Allocator;
use oxc_ast::ast::{NumericLiteral, Str, StringLiteral, TemplateElement};
use oxc_ast_visit::VisitMut;
use oxc_ast_visit::utf8_to_utf16::Utf8ToUtf16;
use serde_json::Value as Json;

use crate::languages::javascript::parse::{parse as parse_program, source_type};

#[derive(Clone, Copy)]
pub struct Node(&'static Json);

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl Eq for Node {}

impl Hash for Node {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(self.0, state);
    }
}

impl fmt::Debug for Node {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.kind(), self.start())
    }
}

pub enum Literal {
    String(&'static str),
    Bool(bool),
    Number(f64),
    Null,
    Other,
}

const INFINITE_RAW: &str = "Infinity";

struct JsonSafe;

impl<'a> VisitMut<'a> for JsonSafe {
    fn visit_numeric_literal(&mut self, literal: &mut NumericLiteral<'a>) {
        if literal.value.is_infinite() {
            literal.value = 0.0;
            literal.raw = Some(Str::from(INFINITE_RAW));
        }
    }

    fn visit_string_literal(&mut self, literal: &mut StringLiteral<'a>) {
        literal.lone_surrogates = false;
    }

    fn visit_template_element(&mut self, element: &mut TemplateElement<'a>) {
        element.lone_surrogates = false;
    }
}

pub fn parse(path: &str, text: &str) -> Option<Node> {
    let allocator = Allocator::default();
    let source = source_type(path);
    let mut parsed = parse_program(&allocator, path, text);

    if !parsed.diagnostics.is_empty() {
        return None;
    }

    Utf8ToUtf16::new(text).convert_program(&mut parsed.program);

    // serde_json rejects the 1e+400 oxc writes for Infinity, so the literal's raw text marks it instead, and rejects the escapes oxc writes for lone surrogates.
    JsonSafe.visit_program(&mut parsed.program);

    let json = parsed.program.to_estree_json(source.is_typescript(), false);
    let mut deserializer = serde_json::Deserializer::from_str(&json);
    deserializer.disable_recursion_limit();
    let root: Json = serde::Deserialize::deserialize(&mut deserializer).unwrap_or_else(|error| {
        panic!("oxc wrote ESTree JSON serde_json rejects for {path}: {error}")
    });

    // The reference keeps every parsed config module for the life of the process, and nodes are compared by address.
    Some(Node(Box::leak(Box::new(root))))
}

fn is_node(value: &Json) -> bool {
    value.get("type").is_some_and(Json::is_string)
        && value.get("start").is_some_and(Json::is_number)
        && value.get("end").is_some_and(Json::is_number)
}

impl Node {
    fn array(self, key: &str) -> &'static [Json] {
        self.0
            .get(key)
            .and_then(Json::as_array)
            .map_or(&[], Vec::as_slice)
    }

    pub fn kind(self) -> &'static str {
        self.text("type").unwrap_or("")
    }

    fn wrap(value: &'static Json) -> Node {
        debug_assert!(is_node(value), "{value} is no node");
        Node(value)
    }

    pub fn field(self, key: &str) -> Option<Node> {
        self.0
            .get(key)
            .filter(|value| !value.is_null())
            .map(Node::wrap)
    }

    pub fn items(self, key: &str) -> impl DoubleEndedIterator<Item = Node> {
        self.array(key)
            .iter()
            .filter(|value| !value.is_null())
            .map(Node::wrap)
    }

    pub fn elements(self) -> impl ExactSizeIterator<Item = Option<Node>> {
        self.array("elements")
            .iter()
            .map(|value| (!value.is_null()).then(|| Node::wrap(value)))
    }

    pub fn text(self, key: &str) -> Option<&'static str> {
        self.0.get(key).and_then(Json::as_str)
    }

    pub fn flag(self, key: &str) -> bool {
        self.0.get(key).and_then(Json::as_bool) == Some(true)
    }

    pub fn start(self) -> u64 {
        self.0.get("start").and_then(Json::as_u64).unwrap_or(0)
    }

    pub fn literal(self) -> Literal {
        if ["regex", "bigint"]
            .iter()
            .any(|key| self.0.get(*key).is_some_and(|value| !value.is_null()))
        {
            return Literal::Other;
        }

        match self.0.get("value") {
            Some(Json::String(value)) => Literal::String(value),
            Some(Json::Bool(value)) => Literal::Bool(*value),
            Some(Json::Number(_)) if self.text("raw") == Some(INFINITE_RAW) => {
                Literal::Number(f64::INFINITY)
            }
            Some(Json::Number(value)) => Literal::Number(value.as_f64().unwrap_or(f64::NAN)),
            _ => Literal::Null,
        }
    }

    pub fn cooked(self) -> Option<&'static str> {
        self.0.get("value")?.get("cooked")?.as_str()
    }

    pub fn children(self) -> Vec<(&'static str, Node)> {
        let mut result = Vec::new();
        let Some(fields) = self.0.as_object() else {
            return result;
        };

        for (key, value) in fields {
            if key == "parent" || key == "hashbang" {
                continue;
            }

            match value {
                Json::Array(values) => result.extend(
                    values
                        .iter()
                        .filter(|child| is_node(child))
                        .map(|child| (key.as_str(), Node(child))),
                ),
                _ if is_node(value) => result.push((key.as_str(), Node(value))),
                _ => {}
            }
        }

        result
    }

    pub fn walk(self, mut enter: impl FnMut(Node)) {
        let mut stack = vec![self];
        while let Some(node) = stack.pop() {
            enter(node);
            stack.extend(node.children().into_iter().rev().map(|(_, child)| child));
        }
    }
}
