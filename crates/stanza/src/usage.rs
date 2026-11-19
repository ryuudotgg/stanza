use serde_json::Value;

use crate::engine::doc::columns;

pub const BOTH_BRACES: &str = "use one of --braces or --no-braces";

pub const USAGE: &str = "Usage: stanza (--fix | --check) [--changed [--hunks] | --stdin <path> | [--] <paths...>] [--json] [--braces | --no-braces]\n       stanza --check --staged [--hunks] [--json] [--braces | --no-braces]\n       stanza explain [--no-braces] [--] <file>:<line>[:<col>]\n       stanza hook [--braces | --no-braces] [--hunks]";

pub const SEE_HELP: &str = "run `stanza --help` for usage";

pub const FLAGS: [[&str; 2]; 13] = [
    ["--fix", "apply every deterministic rule in place"],
    ["--check", "report only, change nothing"],
    [
        "--changed",
        "files from `git diff --name-only HEAD` plus untracked files",
    ],
    [
        "--hunks",
        "with --changed or --staged, only gaps and blocks touching changed lines",
    ],
    [
        "--staged",
        "the staged content of staged files, for pre-commit",
    ],
    [
        "--stdin <path>",
        "source on stdin; --fix: fixed text on stdout, findings on stderr; --check: findings on stdout",
    ],
    ["--json", "findings as a JSON array"],
    [
        "--braces",
        "turn on the braces rule even when lint config turns it off or cannot be read",
    ],
    [
        "--no-braces",
        "turn off the braces rule, keep the blank line rules",
    ],
    [
        "explain <file>:<line>[:<col>]",
        "which rule decides the gap, braced body or wall at that line, and why",
    ],
    [
        "hook",
        "Stop and SubagentStop; PostToolUse on Write, Edit, MultiEdit and Codex `apply_patch`; reads JSON on stdin",
    ],
    ["--help", "usage, flags and the rule catalog"],
    [
        "--version",
        "the version, and for a built binary the commit it was built from",
    ],
];

fn edits(from: &[char], to: &[char]) -> usize {
    let mut rows = vec![(0..=to.len()).collect::<Vec<usize>>()];
    for (i, &a) in from.iter().enumerate() {
        let mut row = vec![i + 1];
        for (j, &b) in to.iter().enumerate() {
            let mut best = (rows[i][j] + usize::from(a != b))
                .min(rows[i][j + 1] + 1)
                .min(row[j] + 1);

            if i > 0 && j > 0 && a == to[j - 1] && from[i - 1] == b {
                best = best.min(rows[i - 1][j - 1] + 1);
            }

            row.push(best);
        }

        rows.push(row);
    }

    rows[from.len()][to.len()]
}

pub fn did_you_mean(arg: &str, takes: impl Fn(&str) -> bool) -> String {
    let candidates = FLAGS
        .iter()
        .filter_map(|[cell, _]| cell.split(' ').next())
        .filter(|flag| flag.starts_with("--") && takes(flag));

    if !arg.starts_with('-') || candidates.clone().any(|flag| flag == arg) {
        return String::new();
    }

    let typed: Vec<char> = arg.chars().collect();
    let mut closest: Option<(usize, &str)> = None;
    for flag in candidates {
        let distance = edits(&typed, &flag.chars().collect::<Vec<_>>());
        if distance <= 2 && closest.is_none_or(|(best, _)| distance < best) {
            closest = Some((distance, flag));
        }
    }

    closest.map_or_else(String::new, |(_, flag)| format!(", did you mean {flag}?"))
}

const WIDTH: usize = 80;

const INDENT: &str = "  ";

fn width(text: &str) -> usize {
    columns(text, 1)
}

fn fill<'a>(words: impl IntoIterator<Item = &'a str>, room: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in words {
        match lines.last_mut() {
            Some(line) if width(line) + 1 + width(word) <= room => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_owned()),
        }
    }

    lines
}

fn hang(first: &str, lines: &[String]) -> Vec<String> {
    if lines.is_empty() {
        return vec![first.trim_end().to_owned()];
    }

    let pad = " ".repeat(width(first));
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let lead = if index == 0 { first } else { &pad };
            format!("{lead}{line}")
        })
        .collect()
}

fn groups(form: &str) -> Vec<&str> {
    let mut depth = 0usize;
    let mut start = 0;
    let mut found = Vec::new();
    for (at, char) in form.char_indices() {
        match char {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            ' ' if depth == 0 => {
                found.push(&form[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }

    found.push(&form[start..]);
    found.retain(|group| !group.is_empty());
    found
}

pub fn usage() -> Vec<String> {
    let lead = width("Usage: ");
    USAGE
        .lines()
        .flat_map(|line| {
            let (first, form) = line.split_at(lead);
            let (program, rest) = form.split_once(' ').unwrap_or((form, ""));
            let first = format!("{first}{program} ");
            hang(
                &first,
                &fill(groups(rest), WIDTH.saturating_sub(width(&first))),
            )
        })
        .collect()
}

pub fn table<const N: usize>(rows: &[[&str; N]]) -> Vec<String> {
    let widths: Vec<usize> = (0..N - 1)
        .map(|index| rows.iter().map(|row| width(row[index])).max().unwrap_or(0))
        .collect();

    rows.iter()
        .flat_map(|row| {
            let mut first = INDENT.to_owned();
            for (cell, column) in row.iter().zip(&widths) {
                first.push_str(cell);
                first.push_str(&" ".repeat(column - width(cell) + 2));
            }

            let room = WIDTH.saturating_sub(width(&first));
            hang(&first, &fill(row[N - 1].split(' '), room))
        })
        .collect()
}

fn compact(value: &Value) -> String {
    match value {
        Value::Object(map) if !map.is_empty() => {
            let entries: Vec<String> = map
                .iter()
                .map(|(key, value)| format!("{}: {}", Value::from(key.as_str()), compact(value)))
                .collect();

            format!("{{ {} }}", entries.join(", "))
        }
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(compact).collect();
            format!("[{}]", items.join(", "))
        }
        _ => value.to_string(),
    }
}

fn layout(value: &Value, indent: usize, before: usize, after: usize) -> String {
    let flat = compact(value);
    if before + width(&flat) + after <= WIDTH {
        return flat;
    }

    let (open, close, items): (char, char, Vec<(String, &Value)>) = match value {
        Value::Object(map) if !map.is_empty() => (
            '{',
            '}',
            map.iter()
                .map(|(key, value)| (format!("{}: ", Value::from(key.as_str())), value))
                .collect(),
        ),
        Value::Array(items) if !items.is_empty() => (
            '[',
            ']',
            items.iter().map(|item| (String::new(), item)).collect(),
        ),
        _ => return flat,
    };

    let inner = indent + INDENT.len();
    let last = items.len() - 1;
    let lines: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(index, (key, value))| {
            let comma = if index < last { "," } else { "" };
            let shown = layout(value, inner, inner + width(key), comma.len());
            format!("{}{key}{shown}{comma}", " ".repeat(inner))
        })
        .collect();

    format!(
        "{open}\n{}\n{}{close}",
        lines.join("\n"),
        " ".repeat(indent)
    )
}

pub fn json(text: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return vec![format!("{INDENT}{text}")];
    };

    let shown = layout(&value, INDENT.len(), INDENT.len(), 0);
    format!("{INDENT}{shown}")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_pads_by_display_width_and_wraps_the_last_cell_at_its_column() {
        let long = "word ".repeat(20);
        let rows = [
            ["a", "x", long.trim_end()],
            ["\u{4e2d}\u{1f600}", "yy", "z"],
        ];

        assert_eq!(
            table(&rows),
            [
                format!("  a     x   {}", "word ".repeat(13).trim_end()),
                format!("            {}", "word ".repeat(7).trim_end()),
                "  \u{4e2d}\u{1f600}  yy  z".to_owned(),
            ]
        );
    }

    #[test]
    fn usage_forms_fit_the_width_and_hang_after_the_program() {
        let lines = usage();
        assert!(lines.iter().all(|line| width(line) <= WIDTH));
        assert_eq!(lines[0], "Usage: stanza (--fix | --check)");
        assert!(lines[1].starts_with("              [--changed [--hunks] |"));
        assert_eq!(
            lines
                .iter()
                .map(|line| line.trim())
                .collect::<Vec<_>>()
                .join(" "),
            USAGE.lines().map(str::trim).collect::<Vec<_>>().join(" ")
        );
    }

    #[test]
    fn json_compacts_back_to_the_registration_and_fits_the_width() {
        let hooks = crate::hook::CLAUDE_CODE_HOOKS;
        let value: Value = serde_json::from_str(hooks).unwrap();
        assert_eq!(compact(&value), hooks);

        let lines = json(hooks);
        assert!(lines.len() > 1);
        assert!(
            lines
                .iter()
                .all(|line| line.starts_with(INDENT) && width(line) <= WIDTH)
        );
        assert_eq!(
            serde_json::from_str::<Value>(&lines.join("\n")).unwrap(),
            value
        );
    }

    #[test]
    fn did_you_mean_names_the_nearest_flag_the_command_takes() {
        let any = |_: &str| true;
        assert_eq!(did_you_mean("--chek", any), ", did you mean --check?");
        assert_eq!(did_you_mean("--jsno", any), ", did you mean --json?");
        assert_eq!(did_you_mean("-check", any), ", did you mean --check?");
        assert_eq!(did_you_mean("--stdn", any), ", did you mean --stdin?");
        assert_eq!(did_you_mean("--hunk", any), ", did you mean --hunks?");
        assert_eq!(did_you_mean("--wat", any), "");
        assert_eq!(did_you_mean("--check", any), "");
        assert_eq!(did_you_mean("chek", any), "");
        assert_eq!(did_you_mean("-h", any), "");
        assert_eq!(did_you_mean("--h", any), "");

        let hook = |flag: &str| ["--braces", "--hunks", "--no-braces"].contains(&flag);
        assert_eq!(did_you_mean("--chek", hook), "");
        assert_eq!(did_you_mean("--brace", hook), ", did you mean --braces?");
    }

    #[test]
    fn did_you_mean_counts_a_swap_as_one_edit_and_breaks_ties_by_table_order() {
        assert_eq!(edits(&['a', 'b'], &['b', 'a']), 1);
        assert_eq!(did_you_mean("--chekc", |_| true), ", did you mean --check?");
        assert_eq!(did_you_mean("--hex", |_| true), ", did you mean --fix?");
        assert_eq!(
            did_you_mean("--hex", |flag| flag != "--fix"),
            ", did you mean --help?"
        );
    }
}
