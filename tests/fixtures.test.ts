import { describe, expect, test } from "bun:test";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { extname, join } from "node:path";
import catalog from "../docs/lib/catalog.json";
import type { BatchRequest, BatchResponse } from "../scripts/batch.ts";
import { preservesShape, preservesText, side } from "../scripts/corpus.ts";
import { batch, collectFiles, gitBinary } from "./support.ts";

const root = join(import.meta.dir, "fixtures");
const extensions = new Set(catalog.extensions);

interface Pair {
  dir: string;
  name: string;
  beforePath: string;
  afterPath: string;
  before: string;
  after: string;
  expectedFindings: string[];
}

const pairs: Pair[] = readdirSync(root, { withFileTypes: true })
  .filter((entry) => entry.isDirectory())
  .map((entry) => entry.name)
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

const requests = pairs.map((pair) => {
  const asked = [
    request(pair.beforePath, pair.before, "fix"),
    request(pair.afterPath, pair.after, "check"),
  ];

  if (pair.before !== pair.after) asked.push(request(pair.beforePath, pair.before, "check"));
  return asked;
});

const answers = batch(root, requests.flat());
const answered = requests.map(
  (asked) => answers.splice(0, asked.length) as [BatchResponse, BatchResponse, BatchResponse?],
);

const fixes = answered.map(([fixed]) => fixed);

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
      const [fixed, afterCheck, beforeCheck] = answered[index]!;

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
        const { error, findings } = afterCheck;
        expect(error).toBeNull();
        expect(findings.filter((f) => f.fixable)).toEqual([]);
        expect(findings.map((f) => `${f.line}:${f.col} ${f.rule}`)).toEqual(expectedFindings);
      });

      if (before !== after)
        test(`${name}: check on before reports fixable findings`, () => {
          expect(beforeCheck!.error).toBeNull();
          expect(beforeCheck!.text).toBe(before);
          expect(beforeCheck!.findings.some((f) => f.fixable)).toBe(true);
        });
    }
  });

const insideGitCheckout =
  Bun.which("git") !== null &&
  Bun.spawnSync([gitBinary, "-C", root, "rev-parse", "--is-inside-work-tree"]).exitCode === 0;

test.skipIf(!insideGitCheckout)("stanza's own file selection never picks the fixtures", () => {
  expect(collectFiles([root], root).files).toEqual([]);
});
