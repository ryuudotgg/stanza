import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { RULES } from "../src/rules.ts";
import { claudeCodeHooks } from "../src/hook.ts";
import { flags } from "../src/usage.ts";

const readme = readFileSync(join(import.meta.dir, "..", "README.md"), "utf8");
const lines = readme.split("\n");

test("the README prints the Claude Code hook registration", () => {
  expect(readme).toContain(claudeCodeHooks);

  const command = [{ type: "command", command: "stanza hook" }];
  expect(JSON.parse(claudeCodeHooks)).toEqual({
    hooks: { PreToolUse: [{ matcher: "Write", hooks: command }], Stop: [{ hooks: command }] },
  });
});

test("the README Use list matches the CLI flag table", () => {
  const start = lines.indexOf("## Use");
  expect(start).toBeGreaterThanOrEqual(0);

  const opening = lines.findIndex((line, index) => index > start && line.startsWith("```"));
  expect(opening).toBeGreaterThan(start);

  const closing = lines.findIndex((line, index) => index > opening && line === "```");
  expect(closing).toBeGreaterThan(opening);

  const rows = lines.slice(opening + 1, closing).map((line) => line.split(/\s{2,}/));
  expect(rows).toEqual(flags.map((flag) => [...flag]));
});

function row(line: string): [string, string] {
  const [id = "", summary = ""] = line
    .split(/(?<!\\)\|/)
    .slice(1, -1)
    .map((cell) => cell.trim().replaceAll("\\|", "|"));

  return [id.replace(/^`(.*)`$/, "$1"), summary];
}

function tableAfter(heading: string): [string, string][] {
  const start = lines.indexOf(heading);
  expect(start).toBeGreaterThanOrEqual(0);

  const header = lines.findIndex((line, index) => index > start && line.startsWith("|"));
  const end = lines.findIndex((line, index) => index > header && !line.startsWith("|"));
  expect(lines[header + 1]).toMatch(/^\|[\s|-]+\|$/);

  return lines.slice(header + 2, end).map(row);
}

function catalog(fixable: boolean): [string, string][] {
  return Object.entries(RULES)
    .filter(([, rule]) => rule.fixable === fixable)
    .map(([id, rule]) => [id, rule.summary]);
}

test("the --fix table matches the fixable rules", () => {
  expect(tableAfter("Applied by `--fix`:")).toEqual(catalog(true));
});

test("the --check table matches the report only rules", () => {
  expect(tableAfter("Reported by `--check`, never fixed:")).toEqual(catalog(false));
});
