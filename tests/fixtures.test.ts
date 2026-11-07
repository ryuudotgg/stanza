import { describe, expect, test } from "bun:test";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { extname, join } from "node:path";
import type { BatchRequest, BatchResponse } from "../scripts/batch.ts";
import { preservesShape, preservesText, side } from "../scripts/corpus.ts";
import { batch, catalog, collectFiles, gitBinary } from "./support.ts";

const root = join(import.meta.dir, "fixtures");
const extensions = new Set(catalog().extensions);

interface Pair {
  dir: string;
  name: string;
  beforePath: string;
  afterPath: string;
  before: string;
  after: string;
  expectedFindings: string[];
}

const pairs: Pair[] = readdirSync(root)
  .sort()
  .flatMap((dir) =>
    readdirSync(join(root, dir))
      .sort()
      .flatMap((file) => {
        const match = /^(.+)\.before(\.[^.]+)$/.exec(file);
        if (!match || !extensions.has(extname(file))) return [];

        const [, name, ext] = match;
        const findingsPath = join(root, dir, `${name}.findings`);
        const afterPath = join(root, dir, `${name}.after${ext}`);
        const beforePath = join(root, dir, file);
        return [
          {
            dir,
            name: name!,
            beforePath,
            afterPath,
            before: readFileSync(beforePath, "utf8"),
            after: readFileSync(afterPath, "utf8"),
            expectedFindings: existsSync(findingsPath)
              ? readFileSync(findingsPath, "utf8").trim().split("\n").filter(Boolean)
              : [],
          },
        ];
      }),
  );

function request(path: string, text: string, mode: BatchRequest["mode"]): BatchRequest {
  return { path, text, mode, braces: null, changedLines: null, explain: [] };
}

const answers = batch(
  root,
  pairs.flatMap((pair) => [
    request(pair.beforePath, pair.before, "fix"),
    request(pair.afterPath, pair.after, "check"),
    request(pair.beforePath, pair.before, "check"),
  ]),
);

const fixes = pairs.map((_, index) => answers[index * 3]!);
const afterChecks = pairs.map((_, index) => answers[index * 3 + 1]!);
const beforeChecks = pairs.map((_, index) => answers[index * 3 + 2]!);

const refixes = batch(
  root,
  pairs.map((pair, index) => request(pair.beforePath, fixes[index]!.text!, "fix")),
);

const parsed = (answer: BatchResponse) =>
  answer.error === null && !answer.findings.some((finding) => finding.rule === "parse");

for (const dir of new Set(pairs.map((pair) => pair.dir)))
  describe(dir, () => {
    for (const [index, pair] of pairs.entries()) {
      if (pair.dir !== dir) continue;

      const { name, beforePath, afterPath, before, after, expectedFindings } = pair;
      const fixed = fixes[index]!;

      test(`${name}: fix matches after`, () => {
        expect(parsed(fixed)).toBe(true);
        expect(fixed.text).toBe(after);
      });

      test(`${name}: fix is idempotent`, () => {
        expect(refixes[index]!.error).toBeNull();
        expect(refixes[index]!.text).toBe(fixed.text);
      });

      test(`${name}: only blank lines and braces change`, () => {
        expect(preservesText(side(beforePath, before), side(afterPath, after))).toBe(true);
      });

      test(`${name}: the program keeps its shape`, () => {
        expect(preservesShape(side(beforePath, before), side(afterPath, after))).toBe(true);
      });

      test(`${name}: check on after reports only report-only findings`, () => {
        const { error, findings } = afterChecks[index]!;
        expect(error).toBeNull();
        expect(findings.filter((f) => f.fixable)).toEqual([]);
        expect(findings.map((f) => `${f.line}:${f.col} ${f.rule}`)).toEqual(expectedFindings);
      });

      if (before !== after)
        test(`${name}: check on before reports fixable findings`, () => {
          const checked = beforeChecks[index]!;
          expect(checked.error).toBeNull();
          expect(checked.text).toBe(before);
          expect(checked.findings.some((f) => f.fixable)).toBe(true);
        });
    }
  });

const insideGitCheckout =
  Bun.which("git") !== null &&
  Bun.spawnSync([gitBinary, "-C", root, "rev-parse", "--is-inside-work-tree"]).exitCode === 0;

test.skipIf(!insideGitCheckout)("stanza's own file selection never picks the fixtures", () => {
  expect(collectFiles([root], root).files).toEqual([]);
});
