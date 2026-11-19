import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { extname, join } from "node:path";
import {
  diffExampleToken,
  diffLines,
  fixturePaths,
  prefixedDiff,
  type DiffLine,
} from "../docs/lib/diff.ts";
import { texts } from "./docs-pages.ts";
import { gitBinary } from "./support.ts";

const fixturesRoot = join(import.meta.dir, "fixtures");

function parsedGitDiff(beforePath: string, afterPath: string): DiffLine[] {
  const result = Bun.spawnSync([
    gitBinary,
    "-c",
    "diff.algorithm=myers",
    "diff",
    "--no-index",
    "--no-color",
    "-U100000",
    beforePath,
    afterPath,
  ]);

  expect([0, 1]).toContain(result.exitCode);

  const lines = result.stdout.toString().split("\n");
  const hunk = lines.findIndex((line) => line.startsWith("@@"));
  if (hunk < 0)
    return readFileSync(beforePath, "utf8")
      .replace(/\n$/, "")
      .split("\n")
      .map((text) => ({ kind: "same", text }));

  return lines
    .slice(hunk + 1)
    .filter((line, index, all) => !(index === all.length - 1 && line === ""))
    .filter((line) => !line.startsWith("\\"))
    .map((line) => ({
      kind: line[0] === "+" ? "add" : line[0] === "-" ? "remove" : "same",
      text: line.slice(1),
    }));
}

for (const [url, page] of Object.entries(texts))
  for (const [, fixture] of page.raw.matchAll(/<DiffExample\s+fixture="([^"]+)"\s*\/>/g))
    test(`${url}: the ${fixture} block is git's diff of the pair`, () => {
      const paths = fixturePaths(fixturesRoot, fixture!);
      const before = readFileSync(paths.before, "utf8");
      const after = readFileSync(paths.after, "utf8");

      const block = prefixedDiff(before, after);
      const fence = `\`\`\`${extname(fixture!).slice(1)} ${diffExampleToken} noCopy\n${block}\n\`\`\``;
      const processedBlock = [
        ...page.processed.matchAll(/^([ \t]*)```([^\n]+)\n([\s\S]*?)^\1```/gm),
      ]
        .map((match) => {
          const indentation = match[1]!;
          const body = match[3]!
            .trimEnd()
            .split("\n")
            .map((line) => (line.startsWith(indentation) ? line.slice(indentation.length) : line))
            .join("\n");

          return { fence: `\`\`\`${match[2]}\n${body}\n\`\`\``, body };
        })
        .find((block) => block.fence === fence);

      expect(processedBlock, `${url} has no processed diff for ${fixture}`).toBeDefined();
      const parsed = processedBlock!.body.split("\n").map((line): DiffLine => ({
        kind: line[0] === "+" ? "add" : line[0] === "-" ? "remove" : "same",
        text: line.slice(1),
      }));

      expect(parsed).toEqual(parsedGitDiff(paths.before, paths.after));
    });

function renderedKinds(html: string): DiffLine["kind"][][] {
  return [...html.matchAll(/<pre\b[\s\S]*?<\/pre>/g)]
    .map(([pre]) =>
      [...pre.matchAll(/<span class="line( diff (add|remove))?"/g)].map((line): DiffLine["kind"] =>
        line[2] === "add" ? "add" : line[2] === "remove" ? "remove" : "same",
      ),
    )
    .filter((kinds) => kinds.some((kind) => kind !== "same"));
}

for (const [url, page] of Object.entries(texts)) {
  const fixtures = [...page.raw.matchAll(/<DiffExample\s+fixture="([^"]+)"\s*\/>/g)].map(
    (match) => match[1]!,
  );

  if (fixtures.length === 0) continue;

  test(`${url}: rendered lines carry the diff marks`, () => {
    const expected = fixtures.map((fixture) => {
      const paths = fixturePaths(fixturesRoot, fixture);
      return diffLines(readFileSync(paths.before, "utf8"), readFileSync(paths.after, "utf8")).map(
        (line) => line.kind,
      );
    });

    expect(renderedKinds(page.html)).toEqual(expected);
  });
}
