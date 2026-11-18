import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import catalog from "../docs/lib/catalog.json";
import { fixturePaths, prefixedDiff } from "../docs/lib/diff.ts";
import { platforms } from "../scripts/packages.ts";
import { pages } from "./docs-pages.ts";
import { blocksAfter, tableAfter } from "./docs-support.ts";

const root = join(import.meta.dir, "..");
const readme = readFileSync(join(root, "README.md"), "utf8");
const lines = readme.split("\n");

test("the README prints the Claude Code hook registration", () => {
  const [block] = blocksAfter(
    readme,
    "To format each Write before it lands, fix each Edit and MultiEdit as soon as it lands, and run a Stop pass at the end of each turn in Claude Code, add the three hooks to `.claude/settings.json`:",
    1,
  );

  expect(block?.opening).toBe("```json");
  expect(JSON.parse(block!.body)).toEqual(JSON.parse(catalog.claudeCodeHooks));

  const command = [{ type: "command", command: "stanza hook" }];
  expect(JSON.parse(catalog.claudeCodeHooks)).toEqual({
    hooks: {
      PreToolUse: [{ matcher: "Write", hooks: command }],
      PostToolUse: [{ matcher: "Edit|MultiEdit", hooks: command }],
      Stop: [{ hooks: command }],
    },
  });
});

test("the README example is the after-guard docs example", () => {
  const paths = fixturePaths(join(root, "tests", "fixtures"), "after-guard/example.ts");
  const rendered = prefixedDiff(
    readFileSync(paths.before, "utf8"),
    readFileSync(paths.after, "utf8"),
  );

  const [block] = blocksAfter(readme, "Before and after `stanza --fix`:", 1);

  const example = readme.split("Before and after `stanza --fix`:")[1]!.split("\n## ")[0]!;

  expect(block?.opening).toBe("```diff");
  expect(block?.body).toBe(`${rendered}\n`);
  expect(example.match(/^```/gm)).toHaveLength(2);
});

test("every level two heading has an emoji and the exit summary is absent", () => {
  const headings = lines.filter((line) => line.startsWith("## "));

  expect(headings.length).toBeGreaterThan(0);
  expect(headings.filter((line) => !/^## \p{Extended_Pictographic}/u.test(line))).toEqual([]);
  expect(readme).not.toContain("Exit 0 when clean");
});

test("the Gatekeeper warning sits in the binaries details as a plain blockquote", () => {
  const details = readme.slice(readme.indexOf("<details>"), readme.indexOf("</details>"));
  expect(details).toMatch(/^> \*\*Warning:\*\*.*Gatekeeper/m);
  expect(details).not.toContain("[!");
});

test("the README release binaries table lists exactly the built assets", () => {
  const assets = tableAfter(readme, ["Asset", "Platform"]).map(([asset]) => asset);
  const built = platforms.map((platform) => `stanza-${platform}`);

  expect(built.length).toBeGreaterThan(0);
  expect(assets.toSorted()).toEqual(built);
});

test("every README link into the site names a page in the site map", () => {
  const links = [...readme.matchAll(/https:\/\/stanza\.ryuu\.gg(\/[^)"\s]*)?/g)].map(
    ([, path]) => path ?? "/",
  );

  expect(links.length).toBeGreaterThan(0);
  expect(links.filter((path) => !pages.includes(path))).toEqual([]);
});
