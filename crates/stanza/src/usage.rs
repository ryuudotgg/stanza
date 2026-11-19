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
        "which rule decides the gap or braced body at that line, and why",
    ],
    [
        "hook",
        "Stop and SubagentStop; PostToolUse on Edit, MultiEdit and Codex `apply_patch`; PreToolUse on Write; reads JSON on stdin",
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

fn units(cell: &str) -> usize {
    cell.encode_utf16().count()
}

pub fn columns<const N: usize>(rows: &[[&str; N]]) -> Vec<String> {
    let widths: Vec<usize> = (0..N)
        .map(|index| rows.iter().map(|row| units(row[index])).max().unwrap_or(0))
        .collect();

    rows.iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(index, cell)| {
                    let padding = if index < N - 1 {
                        widths[index].saturating_sub(units(cell))
                    } else {
                        0
                    };

                    format!("{cell}{}", " ".repeat(padding))
                })
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_pad_every_cell_but_the_last_in_utf16_units() {
        assert_eq!(
            columns(&[["a", "x", "end"], ["\u{1f600}é", "yy", "z"]]),
            ["a    x   end", "\u{1f600}é  yy  z"]
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
