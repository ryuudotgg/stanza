import { expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { preservesShape, side } from "../scripts/corpus.ts";
import { batch } from "./support.ts";

function sources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true, recursive: true })
    .filter(
      (entry) =>
        entry.isFile() && /\.(tsx?|jsx?)$/.test(entry.name) && !entry.name.endsWith(".d.ts"),
    )
    .map((entry) => join(entry.parentPath, entry.name));
}

const root = join(import.meta.dir, "..");
const paths = [...sources(join(root, "tests/fixtures")), ...sources(join(root, "src"))];
const texts = paths.map((path) => readFileSync(path, "utf8"));
const fixed = batch(
  root,
  paths.map((path, index) => ({
    path,
    text: texts[index]!,
    mode: "fix",
    braces: "on",
    changedLines: null,
    explain: [],
  })),
);

for (const [index, path] of paths.entries())
  test(`fix preserves the program: ${path.slice(root.length + 1)}`, () => {
    const answer = fixed[index]!;
    expect(answer.error).toBeNull();
    expect(preservesShape(side(path, texts[index]!), side(path, answer.text!))).toBe(true);
  });
