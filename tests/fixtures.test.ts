import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { collectFiles } from "../src/files.ts";
import { formatText } from "../src/step.ts";
import type { Mode } from "../src/types.ts";

const root = join(import.meta.dir, "fixtures");

function normalize(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.replace(/[{}]/g, "").replace(/\s+/g, " ").trim())
    .filter((line) => line.length > 0);
}

function run(path: string, text: string, mode: Mode) {
  const result = formatText(path, text, { mode });
  return { ...result, text: result.fixed ?? text };
}

for (const dir of readdirSync(root).sort()) {
  const dirPath = join(root, dir);

  describe(dir, () => {
    for (const file of readdirSync(dirPath).sort()) {
      const match = /^(.+)\.before\.(tsx?|jsx?)$/.exec(file);
      if (!match) continue;

      const [, name, ext] = match;
      const beforePath = join(dirPath, file);
      const afterPath = join(dirPath, `${name}.after.${ext}`);
      const findingsPath = join(dirPath, `${name}.findings`);

      const before = readFileSync(beforePath, "utf8");
      const after = readFileSync(afterPath, "utf8");
      const expectedFindings = existsSync(findingsPath)
        ? readFileSync(findingsPath, "utf8").trim().split("\n").filter(Boolean)
        : [];

      test(`${name}: fix matches after`, () => {
        const result = run(beforePath, before, "fix");
        expect(result.parseError).toBe(false);
        expect(result.text).toBe(after);
      });

      test(`${name}: fix is idempotent`, () => {
        const once = run(beforePath, before, "fix");
        const twice = run(beforePath, once.text, "fix");
        expect(twice.text).toBe(once.text);
      });

      test(`${name}: only blank lines and braces change`, () => {
        expect(normalize(after)).toEqual(normalize(before));
      });

      test(`${name}: check on after reports only report-only findings`, () => {
        const result = run(afterPath, after, "check");
        const fixable = result.findings.filter((f) => f.fixable);
        expect(fixable).toEqual([]);

        const reported = result.findings.map((f) => `${f.line}:${f.col} ${f.rule}`);
        expect(reported).toEqual(expectedFindings);
      });

      if (before !== after)
        test(`${name}: check on before reports fixable findings`, () => {
          const result = run(beforePath, before, "check");
          expect(result.text).toBe(before);
          expect(result.findings.some((f) => f.fixable)).toBe(true);
        });
    }
  });
}

const insideGitCheckout =
  Bun.which("git") !== null &&
  Bun.spawnSync(["git", "-C", root, "rev-parse", "--is-inside-work-tree"]).exitCode === 0;

test.skipIf(!insideGitCheckout)("stanza's own file selection never picks the fixtures", () => {
  expect(collectFiles([root], root).files).toEqual([]);
});
