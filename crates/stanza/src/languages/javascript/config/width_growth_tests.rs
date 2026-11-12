use std::sync::Arc;

use super::super::data::Data;
use super::super::tests::{Scratch, dir_with};
use super::{formatter_width, holds_unknown};
use crate::engine::tests::{assert_bounded, cpu_ms, ratio};

fn assert_grows_within(
    sizes: [usize; 2],
    cap: f64,
    build: impl Fn(usize) -> Scratch,
    expected: usize,
) {
    assert_bounded(
        "width",
        &sizes,
        5,
        ratio(cap),
        |size| size,
        |&size| {
            let root = build(size);
            let target = root.join("x.ts");
            let mut columns = 0;
            let ms = cpu_ms(|| columns = formatter_width(&target).columns);
            assert_eq!(columns, expected, "size {size}");
            ms
        },
    );
}

fn doubled_biome_chain(depth: usize) -> Scratch {
    let root = dir_with(&[(
        "biome.json",
        &format!(r#"{{ "extends": ["./c{depth}.json", "./c{depth}.json"] }}"#),
    )]);

    root.write("c0.json", r#"{ "formatter": { "lineWidth": 93 } }"#);
    for level in 1..=depth {
        let below = level - 1;
        root.write(
            &format!("c{level}.json"),
            &format!(r#"{{ "extends": ["./c{below}.json", "./c{below}.json"] }}"#),
        );
    }

    root
}

fn doubled_prettier_options(count: usize) -> Scratch {
    let mut source = "const o0 = { semi: true };\n".to_owned();
    for level in 1..=count {
        let below = level - 1;
        source.push_str(&format!(
            "const o{level} = {{ a: o{below}, b: o{below} }};\n"
        ));
    }

    source.push_str(&format!(
        "export default {{ printWidth: 91, overrides: [{{ files: \"*.ts\", options: o{count} }}] }};\n"
    ));

    dir_with(&[
        (
            "package.json",
            r#"{ "devDependencies": { "prettier": "3" } }"#,
        ),
        ("prettier.config.js", &source),
    ])
}

#[test]
fn biome_extends_doubling_per_level_merges_each_file_once() {
    assert_grows_within([10, 14], 2.0, doubled_biome_chain, 93);
}

#[test]
fn prettier_options_sharing_objects_are_scanned_once() {
    assert_grows_within([10, 20], 3.0, doubled_prettier_options, 91);
}

fn nested(depth: usize, inner: Data) -> Data {
    (0..depth).fold(inner, |value, _| Data::Array(Arc::new(vec![value])))
}

#[test]
fn a_shared_subtree_reached_again_past_the_depth_limit_still_counts() {
    let answers = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(|| {
            let shared = nested(100, Data::Null);
            let fits = Data::Array(Arc::new(vec![
                shared.clone(),
                nested(9_800, shared.clone()),
            ]));
            let overflows = Data::Array(Arc::new(vec![shared.clone(), nested(9_950, shared)]));
            (holds_unknown(&fits), holds_unknown(&overflows))
        })
        .unwrap()
        .join()
        .unwrap();

    assert_eq!(answers, (false, true));
}

#[test]
fn self_referencing_override_options_leave_the_config_unread() {
    let root = dir_with(&[
        (
            "package.json",
            r#"{ "devDependencies": { "prettier": "3" } }"#,
        ),
        (
            "prettier.config.js",
            "const o = { a: o, b: o };\nexport default { printWidth: 91, overrides: [{ files: \"*.ts\", options: o }] };\n",
        ),
    ]);

    let width = formatter_width(&root.join("x.ts"));
    assert_eq!(width.columns, 80);
    assert_eq!(width.unread, [root.join("prettier.config.js")]);
}
