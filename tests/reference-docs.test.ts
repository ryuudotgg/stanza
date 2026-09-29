import { expect, test } from "bun:test";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { flags, usage } from "../src/usage.ts";
import { texts } from "./docs-pages.ts";
import { tableAfter } from "./rule-tables.ts";
import { run, scratch } from "./support.ts";

const docsRoot = join(import.meta.dir, "..", "docs", "content", "docs");
const examplePath = "src/load.ts";

function page(url: string): { raw: string; processed: string; path: string } {
  const text = texts[url];
  expect(text, `${url} has no page text`).toBeDefined();
  return text!;
}

function fence(text: string, title: string): string {
  const lines = text.split("\n");
  const opening = lines.findIndex(
    (line) => line.startsWith("```") && line.includes(`title="${title}"`),
  );

  expect(opening, `no code block titled ${title}`).toBeGreaterThanOrEqual(0);

  const closing = lines.findIndex((line, index) => index > opening && line === "```");
  return lines.slice(opening + 1, closing).join("\n");
}

function example(url: string): Buffer {
  const { raw, path } = page(url);
  const included = /<include\b[^>]*title="src\/load\.ts"[^>]*>\s*([^<]*?)\s*<\/include>/.exec(raw);
  expect(included, `${url} includes no ${examplePath} example`).not.toBeNull();

  return readFileSync(resolve(docsRoot, dirname(path), included![1]!));
}

test("the CLI page lists every flag the CLI has", () => {
  const { processed } = page("/reference/cli");
  expect(tableAfter(processed, ["flag", "what it does"])).toEqual(
    flags.map(([flag, description]) => [flag, description]),
  );

  expect(processed).toContain(`\`\`\`text\n${usage}\n\`\`\``);
});

test("the output page prints the finding stanza prints", () => {
  const url = "/reference/output";
  const stdin = example(url);
  const cwd = scratch("reference");

  const checked = run({ cwd, stdin }, "--check", "--stdin", examplePath);
  expect(checked.code).toBe(1);
  expect(fence(page(url).processed, "Finding")).toBe(checked.stdout.trimEnd());

  const json = run({ cwd, stdin }, "--check", "--json", "--stdin", examplePath);
  expect(JSON.parse(fence(page(url).processed, "--json"))).toEqual(JSON.parse(json.stdout));
});

test("the explain page prints what explain prints", () => {
  const url = "/reference/explain";
  const cwd = scratch("reference");
  mkdirSync(join(cwd, dirname(examplePath)), { recursive: true });
  writeFileSync(join(cwd, examplePath), example(url));

  const explained = run({ cwd }, "explain", `${examplePath}:5`);
  expect(explained.code).toBe(0);
  expect(fence(page(url).processed, "Output")).toBe(explained.stdout.trimEnd());
  expect(page(url).processed).toContain(`\`stanza explain ${examplePath}:5\``);
});
