use super::bun_match;
use crate::engine::tests::{LINEAR_GROWTH_CAP, assert_bounded, cpu_ms, ratio};

#[test]
fn matches_bun_glob() {
    let rows: Vec<(String, String, bool)> =
        serde_json::from_str(include_str!("bun-glob.json")).expect("the table is JSON");

    let wrong: Vec<String> = rows
        .iter()
        .filter(|(pattern, path, expected)| bun_match(pattern, path) != *expected)
        .map(|(pattern, path, expected)| format!("{pattern:?} {path:?} expected {expected}"))
        .collect();

    assert!(
        wrong.is_empty(),
        "{} of {} rows differ:\n{}",
        wrong.len(),
        rows.len(),
        wrong.join("\n")
    );
}

#[test]
fn empty_alternatives_before_a_star_stay_linear_in_the_path() {
    let group = format!("{{{}}}", ",".repeat(99));
    let pattern = format!("{group}{group}*a");
    assert_bounded(
        "bun glob",
        &[1024, 4096],
        5,
        ratio(LINEAR_GROWTH_CAP),
        |length| "b".repeat(length),
        |path| cpu_ms(|| assert!(!bun_match(&pattern, path))),
    );
}
