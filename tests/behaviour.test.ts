import { expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { processFile } from "../src/index.ts";
import { parse } from "../src/parse.ts";

const CONTROLLED: Record<string, string[]> = {
  IfStatement: ["consequent", "alternate"],
  ForStatement: ["body"],
  ForInStatement: ["body"],
  ForOfStatement: ["body"],
  WhileStatement: ["body"],
  DoWhileStatement: ["body"],
};

function shape(node: unknown): unknown {
  if (Array.isArray(node)) return node.map(shape);
  if (node === null || typeof node !== "object") return node;

  const record = node as Record<string, unknown>;
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(record)) {
    if (key === "start" || key === "end") continue;

    const block = value as { type?: string; body?: unknown[] } | null;
    const unwrapped =
      CONTROLLED[String(record.type)]?.includes(key) &&
      block?.type === "BlockStatement" &&
      block.body?.length === 1
        ? block.body[0]
        : value;

    out[key] = shape(unwrapped);
  }

  return out;
}

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
    const before = parse(path, text);
    const fixed = processFile(path, text, "fix", { keepBraces: false });
    const after = parse(path, fixed.text);

    expect(after.errors).toEqual([]);
    expect(shape(after.program)).toEqual(shape(before.program));
  });
