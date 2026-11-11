#![no_main]

use libfuzzer_sys::fuzz_target;
use oxc_allocator::Allocator;
use oxc_ast::ast::NumericLiteral;
use oxc_ast_visit::VisitMut;
use serde::Deserialize;
use serde_json::{Map, Value};
use stanza::engine::process_file;
use stanza::engine::types::{FileResult, Fixed, Mode};
use stanza::languages::Layout;
use stanza::languages::javascript::parse::{parse, source_type};

const EXTENSIONS: [&str; 8] = ["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"];
const COLUMNS: [usize; 4] = [80, 40, 120, 100];
const POSITIONS: [&str; 4] = ["start", "end", "range", "loc"];
const LOOPS: [&str; 5] = [
    "ForStatement",
    "ForInStatement",
    "ForOfStatement",
    "WhileStatement",
    "DoWhileStatement",
];

const STACK: usize = 1 << 30;

#[derive(PartialEq)]
struct Shape {
    tree: Value,
    comments: Vec<String>,
}

struct Case<'a> {
    path: String,
    text: &'a str,
    keep_braces: bool,
    layout: Layout,
}

impl Case<'_> {
    fn run(&self, text: &str, mode: Mode) -> FileResult {
        process_file(&self.path, text, mode, self.keep_braces, self.layout, None)
    }

    fn shape(&self, text: &str) -> Option<Shape> {
        let allocator = Allocator::default();
        let mut parsed = parse(&allocator, &self.path, text);
        if !parsed.diagnostics.is_empty() {
            return None;
        }

        Finite.visit_program(&mut parsed.program);

        let typescript = source_type(&self.path).is_typescript();
        let json = surrogates_as_text(&parsed.program.to_estree_json(typescript, false));
        let mut reader = serde_json::Deserializer::from_str(&json);
        reader.disable_recursion_limit();

        let mut tree = Value::deserialize(&mut reader).expect("oxc writes JSON");
        normalise(&mut tree);

        let comments = parsed
            .program
            .comments
            .iter()
            .map(|comment| comment.span.source_text(text).to_owned())
            .collect();

        Some(Shape { tree, comments })
    }
}

// oxc writes an infinite literal as 1e+400, which serde_json refuses; `raw` still tells them apart.
struct Finite;

impl<'a> VisitMut<'a> for Finite {
    fn visit_numeric_literal(&mut self, literal: &mut NumericLiteral<'a>) {
        if !literal.value.is_finite() {
            literal.value = 0.0;
        }
    }
}

// oxc writes a lone surrogate in a cooked string as a \ud800 escape, which serde_json refuses.
fn surrogates_as_text(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    let mut rest = json;
    while let Some(at) = rest.find('\\') {
        let (before, escape) = rest.split_at(at);
        out.push_str(before);

        let hex = escape.get(2..6).filter(|_| escape[1..].starts_with('u'));
        let surrogate = hex.and_then(|hex| u16::from_str_radix(hex, 16).ok());
        if surrogate.is_some_and(|unit| (0xd800..0xe000).contains(&unit)) {
            out.push('\\');
        }

        let length = escape[1..]
            .chars()
            .next()
            .map_or(1, |char| 1 + char.len_utf8());
        out.push_str(&escape[..length]);
        rest = &escape[length..];
    }

    out.push_str(rest);
    out
}

fn single_block(value: &mut Value) -> Option<Value> {
    let block = value.as_object_mut()?;
    if block.get("type")?.as_str()? != "BlockStatement" {
        return None;
    }

    match block.get_mut("body")?.as_array_mut()?.as_mut_slice() {
        [only] => Some(only.take()),
        _ => None,
    }
}

fn flatten(node: &mut Map<String, Value>, key: &str) {
    if let Some(child) = node.get_mut(key) {
        if let Some(statement) = single_block(child) {
            *child = statement;
        }
    }
}

// Keep in step with shape() in scripts/oracle.ts, which the corpus judge compares the same way.
fn normalise(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(normalise),
        Value::Object(node) => {
            POSITIONS.iter().for_each(|key| {
                node.remove(*key);
            });

            match node.get("type").and_then(Value::as_str) {
                Some("IfStatement") => {
                    flatten(node, "consequent");
                    flatten(node, "alternate");
                }
                Some(kind) if LOOPS.contains(&kind) => flatten(node, "body"),
                _ => {}
            }

            node.values_mut().for_each(normalise);
        }
        _ => {}
    }
}

fn case(bytes: &[u8]) -> Option<Case<'_>> {
    let (&selector, rest) = bytes.split_first()?;
    let text = std::str::from_utf8(rest).ok()?;
    let extension = EXTENSIONS[usize::from(selector & 7)];
    let columns = COLUMNS[usize::from(selector >> 4 & 3)];

    Some(Case {
        path: format!("fuzz.{extension}"),
        text,
        keep_braces: selector & 8 != 0,
        layout: Layout { columns, tab: 2 },
    })
}

fn check(case: &Case) {
    let first = case.run(case.text, Mode::Fix);
    if first.parse_error {
        return;
    }

    let output = match first.fixed {
        Some(Fixed { text, error: None }) => text,
        Some(Fixed {
            error: Some(error), ..
        }) => panic!("the fix does not parse: {error}"),
        None => case.text.to_owned(),
    };

    let again = case.run(&output, Mode::Fix);
    assert!(
        !again.parse_error,
        "stanza rejects its own fix: {:?}",
        again.findings[0].message
    );

    if let Some(again) = again.fixed {
        assert_eq!(again.error, None, "a second fix does not parse");
        assert_eq!(output, again.text, "a second fix changed the text");
    }

    let left: Vec<_> = case
        .run(&output, Mode::Check)
        .findings
        .into_iter()
        .filter(|finding| finding.fixable)
        .map(|finding| format!("{}:{} {}", finding.line, finding.col, finding.message))
        .collect();

    assert!(
        left.is_empty(),
        "fixable findings left after the fix: {left:?}"
    );

    let Some(before) = case.shape(case.text) else {
        return;
    };

    let after = case.shape(&output).expect("the fixed text parses");
    assert!(
        before.comments == after.comments,
        "the fix changed a comment"
    );
    assert!(
        before.tree == after.tree,
        "the fix changed the program's shape"
    );
}

// The CLI formats on a 1 GiB stack; libFuzzer's main thread would cap nesting far lower.
fuzz_target!(|bytes: &[u8]| {
    let Some(case) = case(bytes) else {
        return;
    };

    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn_scoped(scope, || check(&case))
            .expect("the checking thread starts")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });
});
