import { expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { preservesShape, side } from "../scripts/corpus.ts";
import { processFile } from "../src/index.ts";

function sources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true, recursive: true })
    .filter(
      (entry) =>
        entry.isFile() && /\.(tsx?|jsx?)$/.test(entry.name) && !entry.name.endsWith(".d.ts"),
    )
    .map((entry) => join(entry.parentPath, entry.name));
}

const root = join(import.meta.dir, "..");
for (const path of [...sources(join(root, "tests/fixtures")), ...sources(join(root, "src"))])
  test(`fix preserves the program: ${path.slice(root.length + 1)}`, () => {
    const text = readFileSync(path, "utf8");
    const fixed = processFile(path, text, "fix", { keepBraces: false });
    expect(preservesShape(side(path, text), side(path, fixed.text))).toBe(true);
  });

function timedFix(body: string, count: number): { text: string; ms: number } {
  const text = `function f() {\n${body.repeat(count)}}\n`;
  processFile("large.ts", text, "fix", { keepBraces: false });

  let fastest = Infinity;
  let output = "";
  for (let run = 0; run < 3; run++) {
    const started = performance.now();
    output = processFile("large.ts", text, "fix", { keepBraces: false }).text;
    fastest = Math.min(fastest, performance.now() - started);
  }

  return { text: output, ms: fastest };
}

function timedCheck(write: string, read: string): number {
  const text = `function f() {\n  ${write} = 1;\n  if (${read}) run();\n  done();\n  done();\n}\n`;
  processFile("chain.ts", text, "check", { keepBraces: false });

  let fastest = Infinity;
  for (let run = 0; run < 7; run++) {
    const started = performance.now();
    processFile("chain.ts", text, "check", { keepBraces: false });
    fastest = Math.min(fastest, performance.now() - started);
  }

  return fastest;
}

test("fixing thousands of braced bodies stays linear", () => {
  const braced = "  if (a) {\n    f();\n  }\n";
  const small = timedFix(braced, 2_500);
  const large = timedFix(braced, 10_000);
  const unbraced = timedFix("  if (a)\n    f();\n", 10_000);

  expect(large.text).toBe(unbraced.text);
  expect(large.ms).toBeLessThanOrEqual(unbraced.ms * 10);
  expect(large.ms).toBeLessThanOrEqual(small.ms * 8);
}, 30_000);

test("member joins stay linear in chain depth", () => {
  const chain = (depth: number) => `a${".b".repeat(depth)}`;

  expect(timedCheck("a.x", chain(8_000))).toBeLessThanOrEqual(timedCheck("a.x", chain(2_000)) * 6);

  expect(timedCheck(chain(8_000), chain(16_000))).toBeLessThanOrEqual(
    timedCheck(chain(2_000), chain(4_000)) * 6,
  );
}, 30_000);
