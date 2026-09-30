import { expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { preservesShape, side } from "../scripts/corpus.ts";
import { processFile } from "../src/engine/index.ts";

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
    const fixed = processFile(path, text, "fix", {
      keepBraces: false,
      width: { columns: 100, tab: 2 },
    });

    expect(preservesShape(side(path, text), side(path, fixed.text))).toBe(true);
  });

function cpuMs(): number {
  const { user, system } = process.cpuUsage();
  return (user + system) / 1_000;
}

function fastest(
  sizes: number[],
  rounds: number,
  prepare: (size: number) => () => unknown,
): { size: number; ms: number }[] {
  const samples = sizes.map((size) => ({ size, run: prepare(size), ms: Infinity }));
  for (const sample of samples) sample.run();

  for (let round = 0; round < rounds; round++)
    for (const sample of samples) {
      const started = cpuMs();
      sample.run();
      sample.ms = Math.min(sample.ms, cpuMs() - started);
    }

  return samples.map(({ size, ms }) => ({ size, ms }));
}

function growth(samples: { size: number; ms: number }[]): number {
  const points = samples.map(({ size, ms }) => ({ x: Math.log(size), y: Math.log(ms) }));
  const meanX = points.reduce((sum, { x }) => sum + x, 0) / points.length;
  const meanY = points.reduce((sum, { y }) => sum + y, 0) / points.length;

  const covariance = points.reduce((sum, { x, y }) => sum + (x - meanX) * (y - meanY), 0);
  const variance = points.reduce((sum, { x }) => sum + (x - meanX) ** 2, 0);
  return covariance / variance;
}

const body = (statement: string, count: number) => `function f() {\n${statement.repeat(count)}}\n`;
const width = { columns: 80, tab: 2 };
const fix = (text: string) =>
  processFile("large.ts", text, "fix", { keepBraces: false, width }).text;
const check = (text: string) =>
  processFile("chain.ts", text, "check", { keepBraces: false, width });

test("fixing thousands of braced bodies stays linear", () => {
  const braced = "  if (a) {\n    f();\n  }\n";
  const unbraced = "  if (a)\n    f();\n";
  const counts = [1_000, 2_000, 4_000, 8_000];

  const bracedRuns = fastest(counts, 4, (count) => {
    const text = body(braced, count);
    return () => fix(text);
  });

  const [unbracedRun] = fastest([8_000], 4, (count) => {
    const text = body(unbraced, count);
    return () => fix(text);
  });

  const largest = bracedRuns.at(-1);
  if (largest === undefined || unbracedRun === undefined) throw new Error("missing timing run");

  expect(fix(body(braced, 8_000))).toBe(fix(body(unbraced, 8_000)));
  expect(largest.ms).toBeLessThanOrEqual(unbracedRun.ms * 10);
  expect(growth(bracedRuns)).toBeLessThanOrEqual(1.5);
}, 30_000);

test("member joins stay linear in chain depth", () => {
  const chain = (depth: number) => `a${".b".repeat(depth)}`;
  const source = (write: string, read: string) =>
    `function f() {\n  ${write} = 1;\n  if (${read}) run();\n  done();\n  done();\n}\n`;

  const depths = [1_000, 2_000, 4_000, 8_000];
  const reads = fastest(depths, 7, (depth) => {
    const text = source("a.x", chain(depth));
    return () => check(text);
  });

  const joins = fastest(depths, 7, (depth) => {
    const text = source(chain(depth), chain(depth * 2));
    return () => check(text);
  });

  expect(growth(reads)).toBeLessThanOrEqual(1.5);
  expect(growth(joins)).toBeLessThanOrEqual(1.5);
}, 30_000);
