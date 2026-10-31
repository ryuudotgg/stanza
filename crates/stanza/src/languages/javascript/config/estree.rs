use std::collections::HashSet;

use oxc_allocator::Allocator;
use oxc_ast::ast::{NumericLiteral, StringLiteral, TemplateElement};
use oxc_ast_visit::VisitMut;
use oxc_ast_visit::utf8_to_utf16::Utf8ToUtf16;
use serde_json::Value as Json;

use crate::languages::javascript::parse::{parse as parse_program, source_type};

pub type Node = Json;

pub struct Program {
    pub root: &'static Node,
    pub infinite: HashSet<u64>,
}

#[derive(Default)]
struct JsonSafe {
    infinite: HashSet<u64>,
}

impl<'a> VisitMut<'a> for JsonSafe {
    fn visit_numeric_literal(&mut self, literal: &mut NumericLiteral<'a>) {
        if literal.value.is_infinite() {
            self.infinite.insert(u64::from(literal.span.start));
            literal.value = 0.0;
        }
    }

    fn visit_string_literal(&mut self, literal: &mut StringLiteral<'a>) {
        literal.lone_surrogates = false;
    }

    fn visit_template_element(&mut self, element: &mut TemplateElement<'a>) {
        element.lone_surrogates = false;
    }
}

pub fn parse(path: &str, text: &str) -> Option<Program> {
    let allocator = Allocator::default();
    let source = source_type(path);
    let mut parsed = parse_program(&allocator, path, text);

    if !parsed.diagnostics.is_empty() {
        return None;
    }

    Utf8ToUtf16::new(text).convert_program(&mut parsed.program);

    // serde_json rejects the 1e+400 oxc writes for Infinity and the escapes it writes for lone surrogates.
    let mut safe = JsonSafe::default();
    safe.visit_program(&mut parsed.program);

    let json = parsed.program.to_estree_json(source.is_typescript(), false);
    let mut deserializer = serde_json::Deserializer::from_str(&json);
    deserializer.disable_recursion_limit();
    let root: Node = serde::Deserialize::deserialize(&mut deserializer).unwrap_or_else(|error| {
        panic!("oxc wrote ESTree JSON serde_json rejects for {path}: {error}")
    });

    // The reference keeps every parsed config module for the life of the process, and nodes are compared by address.
    Some(Program {
        root: Box::leak(Box::new(root)),
        infinite: safe.infinite,
    })
}

pub fn kind(node: &Node) -> &str {
    node.get("type").and_then(Json::as_str).unwrap_or("")
}

pub fn field<'a>(node: &'a Node, key: &str) -> Option<&'a Node> {
    node.get(key).filter(|value| !value.is_null())
}

pub fn items<'a>(node: &'a Node, key: &str) -> &'a [Node] {
    node.get(key)
        .and_then(Json::as_array)
        .map_or(&[], Vec::as_slice)
}

pub fn text<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    node.get(key).and_then(Json::as_str)
}

pub fn flag(node: &Node, key: &str) -> bool {
    node.get(key).and_then(Json::as_bool) == Some(true)
}

pub fn start(node: &Node) -> u64 {
    node.get("start").and_then(Json::as_u64).unwrap_or(0)
}

pub fn is_node(value: &Json) -> bool {
    value.get("type").is_some_and(Json::is_string)
        && value.get("start").is_some_and(Json::is_number)
        && value.get("end").is_some_and(Json::is_number)
}

pub fn same(left: &Node, right: &Node) -> bool {
    std::ptr::eq(left, right)
}

pub fn children(node: &Node) -> Vec<(&str, &Node)> {
    let mut result = Vec::new();
    let Some(fields) = node.as_object() else {
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
                    .map(|child| (key.as_str(), child)),
            ),
            _ if is_node(value) => result.push((key.as_str(), value)),
            _ => {}
        }
    }

    result
}

pub fn walk<'a>(root: &'a Node, mut enter: impl FnMut(&'a Node)) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        enter(node);
        stack.extend(children(node).into_iter().rev().map(|(_, child)| child));
    }
}
