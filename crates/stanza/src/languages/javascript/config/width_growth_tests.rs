use std::cell::Cell;

use super::super::growth_tests::{assert_linear, numbered};
use super::super::tests::{Scratch, dir_with};
use super::formatter_width;
use super::tests::BIOME_READS;
use crate::testing::timing::{assert_bounded, cpu_ms, ratio};

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

#[test]
fn biome_extends_doubling_per_level_merges_each_file_once() {
    assert_grows_within([4, 8], 2.0, doubled_biome_chain, 93);
}

fn chain_link(index: usize, target: &str) -> (String, String) {
    (
        format!("c{index}.json"),
        format!(r#"{{ "extends": ["{target}"] }}"#),
    )
}

fn chain_tail(files: usize) -> Scratch {
    let tail = dir_with(&[(
        &format!("c{files}.json"),
        r#"{ "formatter": { "lineWidth": 90 } }"#,
    )]);

    for index in 12..files {
        let (name, text) = chain_link(index, &format!("./c{}.json", index + 1));
        tail.write(&name, &text);
    }

    tail
}

fn chain_head(tail: &Scratch) -> Scratch {
    let head = dir_with(&[("biome.json", r#"{ "extends": ["./c1.json"] }"#)]);
    for index in 1..=11 {
        let target = if index == 11 {
            tail.join("c12.json")
        } else {
            format!("./c{}.json", index + 1)
        };

        let (name, text) = chain_link(index, &target);
        head.write(&name, &text);
    }

    head
}

#[test]
fn biome_width_chain_budget() {
    assert_bounded(
        "biome chain",
        &[1000, 4000],
        5,
        ratio(8.0),
        chain_tail,
        |tail| {
            let head = chain_head(tail);
            let target = head.join("x.ts");
            BIOME_READS.with(|reads| reads.set(0));
            let mut width = None;
            let ms = cpu_ms(|| width = Some(formatter_width(&target)));
            let width = width.unwrap();
            assert_eq!(
                (width.columns, width.unread),
                (80, vec![head.join("biome.json")])
            );
            assert!(BIOME_READS.with(Cell::get) <= 11);
            ms
        },
    );
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

#[test]
fn prettier_overrides_with_doubled_spread_options_stay_linear() {
    assert_linear(
        500,
        |n| {
            let chain = numbered(n, |k| {
                format!("const x{k} = {{ ...x{0}, ...x{0} }};\n", k - 1)
            })
            .concat();

            let overrides =
                numbered(n, |k| format!("{{ files: \"*.none\", options: x{k} }}")).join(", ");

            vec![
                ("a.ts".to_owned(), String::new()),
                (
                    "package.json".to_owned(),
                    r#"{ "devDependencies": { "prettier": "3" } }"#.to_owned(),
                ),
                (
                    "prettier.config.mjs".to_owned(),
                    format!(
                        "const x0 = {{}};\n{chain}export default {{ overrides: [{overrides}] }};\n"
                    ),
                ),
            ]
        },
        |file| {
            formatter_width(file);
        },
    );
}

#[test]
fn reading_every_key_of_many_one_key_spreads_stays_linear() {
    assert_linear(
        2_000,
        |n| {
            let spreads = numbered(n, |k| format!("...{{ k{k}: 1 }}")).join(", ");
            vec![
                ("a.ts".to_owned(), String::new()),
                (
                    "package.json".to_owned(),
                    r#"{ "devDependencies": { "prettier": "3" } }"#.to_owned(),
                ),
                (
                    "prettier.config.mjs".to_owned(),
                    format!(
                        "export default {{ overrides: [{{ files: \"*.ts\", options: {{ {spreads} }} }}] }};\n"
                    ),
                ),
            ]
        },
        |file| {
            formatter_width(file);
        },
    );
}

#[test]
fn one_key_spreads_after_a_doubled_spread_chain_stay_linear() {
    assert_linear(
        2_000,
        |n| {
            let doubled = numbered(64, |k| {
                format!("const x{k} = {{ ...x{0}, ...x{0} }};\n", k - 1)
            })
            .concat();

            let spreads = numbered(n, |k| format!("...{{ k{k}: 1 }}")).join(", ");
            vec![
                ("a.ts".to_owned(), String::new()),
                (
                    "package.json".to_owned(),
                    r#"{ "devDependencies": { "prettier": "3" } }"#.to_owned(),
                ),
                (
                    "prettier.config.mjs".to_owned(),
                    format!(
                        "const x0 = {{ padding: 1 }};\n{doubled}export default {{ overrides: [{{ files: \"*.ts\", options: {{ ...x64, {spreads} }} }}] }};\n"
                    ),
                ),
            ]
        },
        |file| {
            formatter_width(file);
        },
    );
}
