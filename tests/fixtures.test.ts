import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { entryFor } from "../src/languages/index.ts";
import { isIdempotent, preservesShape, preservesText, side } from "../scripts/corpus.ts";
import { collectFiles } from "../src/files.ts";
import { formatText, stepSettings } from "../src/step.ts";
import type { Mode } from "../src/engine/types.ts";

const root = join(import.meta.dir, "fixtures");

function run(path: string, text: string, mode: Mode) {
  const result = formatText(path, text, { mode });
  return { ...result, text: result.fixed ?? text };
}

for (const dir of readdirSync(root).sort()) {
  const dirPath = join(root, dir);

  describe(dir, () => {
    for (const file of readdirSync(dirPath).sort()) {
      const match = /^(.+)\.before(\.[^.]+)$/.exec(file);
      if (!match || entryFor(file) === undefined) continue;

      const [, name, ext] = match;
      const beforePath = join(dirPath, file);
      const afterPath = join(dirPath, `${name}.after${ext}`);
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
        expect(
          isIdempotent(beforePath, run(beforePath, before, "fix").text, {
            ...stepSettings(beforePath),
          }),
        ).toBe(true);
      });

      test(`${name}: only blank lines and braces change`, () => {
        expect(preservesText(side(beforePath, before), side(afterPath, after))).toBe(true);
      });

      test(`${name}: the program keeps its shape`, () => {
        expect(preservesShape(side(beforePath, before), side(afterPath, after))).toBe(true);
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
