export const usage =
  "Usage: stanza (--fix | --check) [--changed [--hunks] | --stdin <path> | [--] <paths...>] [--json] [--braces | --no-braces]\n       stanza --check --staged [--hunks] [--json] [--braces | --no-braces]\n       stanza explain <file>:<line> [--no-braces]\n       stanza hook [--braces | --no-braces] [--hunks]";

export const flags: (readonly [string, string])[] = [
  ["--fix", "apply every deterministic rule in place"],
  ["--check", "report only, change nothing"],
  ["--changed", "files from `git diff --name-only HEAD` plus untracked files"],
  ["--hunks", "with --changed or --staged, only gaps and blocks touching changed lines"],
  ["--staged", "the staged content of staged files, for pre-commit"],
  [
    "--stdin <path>",
    "source on stdin; --fix: fixed text on stdout, findings on stderr; --check: findings on stdout",
  ],
  ["--json", "findings as a JSON array, for hooks"],
  ["--braces", "turn on the braces rule even when lint config turns it off or cannot be read"],
  ["--no-braces", "turn off the braces rule, keep the blank line rules"],
  ["explain <file>:<line>", "which rule decides the gap or braced body at that line, and why"],
  ["hook", "the Stop hook and PreToolUse hook on Write, reads JSON on stdin"],
  ["--help", "usage, flags and the rule catalog"],
  ["--version", "the version, and for a built binary the commit it was built from"],
];

export function columns(rows: readonly (readonly string[])[]): string[] {
  const widths = rows[0]!.map((_, index) => Math.max(...rows.map((row) => row[index]!.length)));
  return rows.map((row) =>
    row
      .map((cell, index) => (index < row.length - 1 ? cell.padEnd(widths[index]!) : cell))
      .join("  "),
  );
}
