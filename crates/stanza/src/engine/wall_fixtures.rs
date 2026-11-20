pub(super) fn guards(count: usize, split: bool) -> Vec<String> {
    (0..count)
        .map(|index| {
            let gap = if split && index == 3 { "\n" } else { "" };
            format!("{gap}  if (status === {}) return \"{index}\";", 401 + index)
        })
        .collect()
}

pub(super) fn calls(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("  console.log({index});"))
        .collect()
}

pub(super) fn wall_body(lines: &[String]) -> String {
    format!("function run(status, rows) {{\n{}\n}}\n", lines.join("\n"))
}

pub(super) fn detached_wall() -> Vec<String> {
    [
        "  if (ready) consume(0);",
        "  if (ready) consume(1);",
        "  if (ready) consume(2);",
        "  // section",
        "",
        "  if (ready) consume(3);",
        "  if (ready) consume(4);",
        "  if (ready) consume(5);",
        "  consume(6);",
        "  consume(7);",
        "  consume(8);",
    ]
    .map(str::to_owned)
    .into()
}
