use serde_json::Value;

use super::super::object::ordered_keys;
use super::super::tests::dir_with;
use super::*;

struct Counted<R> {
    inner: R,
    bytes: usize,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.bytes += count;
        Ok(count)
    }
}

fn same(tree: &Tree, expected: &Value) -> bool {
    match (tree, expected) {
        (Tree::Null, Value::Null) => true,
        (Tree::Bool(ours), Value::Bool(theirs)) => ours == theirs,
        (Tree::Number(ours), Value::Number(theirs)) => theirs.as_f64() == Some(*ours),
        (Tree::String(ours), Value::String(theirs)) => ours == theirs,
        (Tree::Array(ours), Value::Array(theirs)) => {
            ours.len() == theirs.len()
                && ours
                    .iter()
                    .zip(theirs)
                    .all(|(ours, theirs)| same(ours, theirs))
        }
        (Tree::Object(ours), Value::Object(theirs)) => {
            let ordered = ordered_keys(ours.keys());
            let expected: Vec<&String> = theirs.keys().collect();

            ordered.iter().eq(expected)
                && ours
                    .iter()
                    .all(|(key, value)| theirs.get(key).is_some_and(|theirs| same(value, theirs)))
        }
        _ => false,
    }
}

fn nested_maps(depth: usize) -> String {
    let lines: Vec<String> = (0..depth)
        .map(|level| format!("{}k:", "  ".repeat(level)))
        .collect();

    format!("{} 0\n", lines.join("\n"))
}

fn zeros(count: usize) -> String {
    format!("extra: [{}]\n", vec!["0"; count].join(","))
}

fn comment(bytes: usize) -> String {
    format!("#{}", "x".repeat(bytes - 1))
}

#[test]
fn bounded_yaml_rows_match_the_reference_parser() {
    let rows: Vec<Value> =
        serde_json::from_str(include_str!("yaml-rows.json")).expect("the table is JSON");

    assert_eq!(rows.len(), 41);

    for row in &rows {
        let name = &row["name"];
        let input = row["input"].as_str().expect("a row has an input");
        let read = parse(input.as_bytes());
        match row["reader"].as_str() {
            Some("tree") => {
                let expected = row["parser"]
                    .get("tree")
                    .expect("the reference parser reads every tree row");

                assert!(
                    read.as_ref().is_some_and(|tree| same(tree, expected)),
                    "{name}: {read:?}"
                );
            }
            Some("unknown") => assert!(read.is_none(), "{name}: {read:?}"),
            other => panic!("{name}: reader {other:?}"),
        }
    }
}

#[test]
fn bounded_yaml_limits_reject_one_past_each_cap() {
    assert!(parse(nested_maps(64).as_bytes()).is_some());
    assert!(parse(nested_maps(65).as_bytes()).is_none());

    assert!(parse(zeros(99_997).as_bytes()).is_some());
    assert!(parse(zeros(99_998).as_bytes()).is_none());
    assert!(parse(zeros(100_001).as_bytes()).is_none());

    assert_eq!(parse(comment(MAX_BYTES).as_bytes()), Some(Tree::Null));
    assert!(parse(comment(MAX_BYTES + 1).as_bytes()).is_none());
}

#[test]
fn bounded_yaml_limits_reject_block_keys_past_the_reference_span() {
    let key = |length: usize| "k".repeat(length);

    for (input, expected) in [
        (format!("{}: 1\n", key(MAX_KEY_SPAN)), true),
        (format!("{}: 1\n", key(MAX_KEY_SPAN + 1)), false),
        (format!("\"{}\": 1\n", key(MAX_KEY_SPAN - 2)), true),
        (format!("\"{}\": 1\n", key(MAX_KEY_SPAN + 6)), false),
        (format!("- {}: 1\n", key(MAX_KEY_SPAN)), true),
        (format!("- {}: 1\n", key(MAX_KEY_SPAN + 1)), false),
    ] {
        assert_eq!(
            parse(input.as_bytes()).is_some(),
            expected,
            "{}",
            input.len()
        );
    }
}

#[test]
fn bounded_yaml_limits_hold_in_flow_and_mixed_nesting() {
    let flow = |depth: usize| format!("{}{}\n", "[".repeat(depth), "]".repeat(depth));
    let mixed = |depth: usize| format!("{}{}\n", "- ".repeat(depth - 1), "[0]");

    assert!(parse(flow(64).as_bytes()).is_some());
    assert!(parse(flow(65).as_bytes()).is_none());
    assert!(parse(mixed(64).as_bytes()).is_some());
    assert!(parse(mixed(65).as_bytes()).is_none());
}

#[test]
fn bounded_yaml_limits_cap_what_the_loader_reads() {
    let mut endless = Counted {
        inner: io::repeat(b'#'),
        bytes: 0,
    };

    let read = capped(&mut endless).expect("an endless source reads");
    assert_eq!(endless.bytes, MAX_BYTES + 1);
    assert!(parse(&read).is_none());

    let scratch = dir_with(&[
        ("at.yaml", &comment(MAX_BYTES)),
        ("past.yaml", &comment(MAX_BYTES + 1)),
    ]);

    assert_eq!(load(&scratch.join("at.yaml")), Some(Tree::Null));
    assert!(load(&scratch.join("past.yaml")).is_none());
    assert!(load(&scratch.join("missing.yaml")).is_none());
}

#[test]
fn invalid_utf8_anywhere_is_unknown() {
    assert!(parse(b"a: \xff\n").is_none());
    assert!(parse(b"a: 1\n# \xff\n").is_none());
}

#[test]
fn raw_characters_outside_the_printable_set_are_unknown() {
    for input in [
        "a: x\u{0}y\n",
        "a: \"x\u{1}y\"\n",
        "a: 1 # \u{1b}\n",
        "a: x\u{7f}\n",
        "a: x\u{9b}\n",
        "a: x\u{fffe}\n",
    ] {
        assert!(parse(input.as_bytes()).is_none(), "{input:?}");
    }

    assert_eq!(
        parse(b"a: \"x\\u0000y\"\nb: x\ty # \t\n"),
        Some(Tree::Object(
            [
                ("a".to_owned(), Tree::String("x\u{0}y".to_owned())),
                ("b".to_owned(), Tree::String("x\ty".to_owned())),
            ]
            .into_iter()
            .collect()
        ))
    );
}

#[test]
fn layouts_the_reference_parser_accepts_read_the_same() {
    let object = |entries: Vec<(&str, Tree)>| {
        Tree::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    };

    let text = |value: &str| Tree::String(value.to_owned());

    for (input, expected) in [
        (
            "extends:\n- a\n- b\nroot: true\n",
            object(vec![
                ("extends", Tree::Array(vec![text("a"), text("b")])),
                ("root", Tree::Bool(true)),
            ]),
        ),
        (
            "extends: [\n  a,\n  b\n]\n",
            object(vec![("extends", Tree::Array(vec![text("a"), text("b")]))]),
        ),
        (
            "{\n  \"printWidth\": 100\n}\n",
            object(vec![("printWidth", Tree::Number(100.0))]),
        ),
        (
            "a: 1\n  # aside\nb:\n",
            object(vec![("a", Tree::Number(1.0)), ("b", Tree::Null)]),
        ),
    ] {
        assert_eq!(parse(input.as_bytes()), Some(expected), "{input:?}");
    }
}

#[test]
fn layouts_outside_the_subset_are_unknown() {
    for input in [
        "a:\n  value\n",
        "a:\n#c\n  x\nb: 1\n",
        "a: [x,\ny]\n",
        "{a: 1\n# c\n}\n",
        "\u{feff}a: 1\n",
        "a: {b}\n",
        "a: [b: c]\n",
        "a: \"\\x41\"\n",
        "a: \"\\ud83d\"\n",
        "a: 123456789012345678901\n",
        "a: 1e400\n",
        "null: 1\n",
        "<<: {a: 1}\n",
        "a: b: c\n",
        "- a\nb: 1\n",
    ] {
        assert!(parse(input.as_bytes()).is_none(), "{input:?}");
    }
}
