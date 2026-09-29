import { expect } from "bun:test";
import { RULES } from "../src/engine/rules.ts";

export function row(line: string): [string, string] {
  const [id = "", summary = ""] = line
    .split(/(?<!\\)\|/)
    .slice(1, -1)
    .map((cell) => cell.trim().replaceAll("\\|", "|"));

  return [id.replace(/^\[`(.*)`\]\([^)]*\)$/, "$1").replace(/^`(.*)`$/, "$1"), summary];
}

export function catalog(fixable: boolean): [string, string][] {
  return Object.entries(RULES)
    .filter(([, rule]) => rule.fixable === fixable)
    .map(([id, rule]) => [id, rule.summary]);
}

export function tableAfter(text: string, header: [string, string]): [string, string][] {
  const lines = text.split("\n");
  const start = lines.findIndex((line) => row(line).join("|") === header.join("|"));
  expect(start, `${header.join(" | ")} table is missing`).toBeGreaterThanOrEqual(0);
  expect(lines[start + 1]).toMatch(/^\|[\s|:-]+\|$/);

  const end = lines.findIndex((line, index) => index > start + 1 && !line.startsWith("|"));
  return lines.slice(start + 2, end < 0 ? undefined : end).map(row);
}
