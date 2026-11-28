import { expect } from "bun:test";
import catalog from "../docs/lib/catalog.json";
import { texts } from "./docs-pages.ts";

type Pair = [string, string];

export function page(url: string): { raw: string; processed: string; path: string } {
  const text = texts[url];
  expect(text, `${url} has no page text`).toBeDefined();
  return text!;
}

export function section(url: string, heading: string): string {
  const content = page(url).raw.split(`## ${heading}\n`)[1]?.split("\n## ")[0];
  expect(content, `${url} has no ${heading} section`).toBeDefined();
  return content!;
}

export function fence(text: string, title: string): string {
  const lines = text.split("\n");
  const opening = lines.findIndex(
    (line) => /^\s*```/.test(line) && line.includes(`title="${title}"`),
  );

  expect(opening, `no code block titled ${title}`).toBeGreaterThanOrEqual(0);

  const indentation = /^\s*/.exec(lines[opening]!)![0];
  const closing = lines.findIndex(
    (line, index) => index > opening && line === `${indentation}\`\`\``,
  );

  expect(closing, `no closing fence for ${title}`).toBeGreaterThan(opening);

  return lines
    .slice(opening + 1, closing)
    .map((line) => (line.startsWith(indentation) ? line.slice(indentation.length) : line))
    .join("\n");
}

export function openingFence(text: string): string {
  const body = /^\s*```[^\n]*\n([\s\S]*?)\n```\n/.exec(text)?.[1];
  expect(body, "the page opens with no code block").toBeDefined();
  return body!;
}

function cells(line: string): string[] {
  return line
    .trim()
    .split(/(?<!\\)\|/)
    .slice(1, -1)
    .map((cell) => cell.trim().replaceAll("\\|", "|"));
}

function pair([id = "", summary = ""]: string[]): Pair {
  return [id.replace(/^\[`(.*)`\]\([^)]*\)$/, "$1").replace(/^`(.*)`$/, "$1"), summary];
}

export function row(line: string): Pair {
  return pair(cells(line));
}

export function ruleRows(fixable: boolean): Pair[] {
  return Object.entries(catalog.rules)
    .filter(([, rule]) => rule.fixable === fixable)
    .map(([id, rule]) => [id, rule.summary]);
}

function rowsAfter(lines: string[], start: number): string[][] {
  expect(lines[start + 1]?.trim()).toMatch(/^\|[\s|:-]+\|$/);
  const end = lines.findIndex((line, index) => index > start + 1 && !line.trim().startsWith("|"));
  return lines.slice(start + 2, end < 0 ? undefined : end).map(cells);
}

function tableStarts(
  lines: string[],
  header: string[],
  read: (line: string) => string[],
): number[] {
  const starts = lines.flatMap((line, index) =>
    Bun.deepEquals(read(line), header) ? [index] : [],
  );

  expect(starts.length, `${header.join(" | ")} table is missing`).toBeGreaterThan(0);
  return starts;
}

export function rawTableAfter(text: string, header: string[]): string[][] {
  const lines = text.split("\n");
  return rowsAfter(lines, tableStarts(lines, header, cells)[0]!);
}

export function tableAfter(text: string, header: Pair): Pair[] {
  const lines = text.split("\n");
  return rowsAfter(lines, tableStarts(lines, header, row)[0]!).map(pair);
}

export function everyTableWithHeader(text: string, header: Pair): Pair[] {
  const lines = text.split("\n");
  return tableStarts(lines, header, row).flatMap((start) => rowsAfter(lines, start).map(pair));
}

export function blocksAfter(
  text: string,
  marker: string,
  count: number,
): { opening: string; body: string }[] {
  const lines = text.split("\n");
  let cursor = lines.indexOf(marker);
  expect(cursor, `${marker} is missing`).toBeGreaterThanOrEqual(0);

  return Array.from({ length: count }, () => {
    const opening = lines.findIndex((line, index) => index > cursor && line.startsWith("```"));
    const closing = lines.findIndex((line, index) => index > opening && line === "```");
    expect(closing).toBeGreaterThan(opening);

    cursor = closing;
    return { opening: lines[opening]!, body: `${lines.slice(opening + 1, closing).join("\n")}\n` };
  });
}
