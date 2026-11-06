pub const BOTH_BRACES: &str = "use one of --braces or --no-braces";

pub const USAGE: &str = "Usage: stanza (--fix | --check) [--changed [--hunks] | --stdin <path> | [--] <paths...>] [--json] [--braces | --no-braces]\n       stanza --check --staged [--hunks] [--json] [--braces | --no-braces]\n       stanza explain [--no-braces] [--] <file>:<line>\n       stanza hook [--braces | --no-braces] [--hunks]";

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
        "explain <file>:<line>",
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
}
