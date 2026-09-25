import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { RULES } from "../src/rules.ts";

const lines = readFileSync(join(import.meta.dir, "..", "README.md"), "utf8").split("\n");

function row(line: string): [string, string] {
  const [id = "", summary = ""] = line
    .split("|")
    .slice(1, -1)
    .map((cell) => cell.trim());

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
