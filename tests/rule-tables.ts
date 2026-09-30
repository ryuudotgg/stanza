import { expect } from "bun:test";
import { RULES } from "../src/engine/rules.ts";

export function row(line: string): [string, string] {
  const [id = "", summary = ""] = line
    .trim()
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

function rowsAfter(lines: string[], start: number): [string, string][] {
  expect(lines[start + 1]?.trim()).toMatch(/^\|[\s|:-]+\|$/);
  const end = lines.findIndex((line, index) => index > start + 1 && !line.trim().startsWith("|"));
  return lines.slice(start + 2, end < 0 ? undefined : end).map(row);
}

function tableStarts(lines: string[], header: [string, string]): number[] {
  const starts = lines.flatMap((line, index) =>
    row(line).join("|") === header.join("|") ? [index] : [],
  );

  expect(starts.length, `${header.join(" | ")} table is missing`).toBeGreaterThan(0);
  return starts;
}

export function tableAfter(text: string, header: [string, string]): [string, string][] {
  const lines = text.split("\n");
  return rowsAfter(lines, tableStarts(lines, header)[0]!);
}

export function everyTableWithHeader(text: string, header: [string, string]): [string, string][] {
  const lines = text.split("\n");
  return tableStarts(lines, header).flatMap((start) => rowsAfter(lines, start));
}
