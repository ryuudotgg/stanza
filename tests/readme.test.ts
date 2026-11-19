import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import catalog from "../docs/lib/catalog.json";
import { fixturePaths, prefixedDiff } from "../docs/lib/diff.ts";
import { printJson } from "../docs/lib/hooks.ts";
import { platforms } from "../scripts/packages.ts";
import { pages } from "./docs-pages.ts";
import { blocksAfter, tableAfter } from "./docs-support.ts";

const root = join(import.meta.dir, "..");
const readme = readFileSync(join(root, "README.md"), "utf8");
const lines = readme.split("\n");

test("the first README block under Hooks is the Claude Code registration", () => {
  const [block] = blocksAfter(readme, "### Hooks", 1);
  const { settings } = catalog.registrations["Claude Code"];

  expect(block?.opening).toBe("```json");
  expect(block!.body).toBe(`${printJson(settings)}\n`);

  const command = [{ type: "command", command: "stanza hook" }];
  expect(settings).toEqual({
    hooks: {
      PostToolUse: [{ matcher: "Write|Edit|MultiEdit", hooks: command }],
      Stop: [{ hooks: command }],
      SubagentStop: [{ hooks: command }],
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

test("every level two heading starts with an emoji", () => {
  const headings = lines.filter((line) => line.startsWith("## "));
  expect(headings.length).toBeGreaterThan(0);
  expect(headings.filter((line) => !/^## \p{Extended_Pictographic}/u.test(line))).toEqual([]);
});

test("the binaries details warn about Gatekeeper in a blockquote", () => {
  const details = readme.slice(readme.indexOf("<details>"), readme.indexOf("</details>"));
  expect(details).toMatch(/^> \*\*Warning:\*\*.*Gatekeeper/m);
});

test("the README release binaries table lists exactly the built assets", () => {
  const assets = tableAfter(readme, ["Asset", "Platform"]).map(([asset]) => asset);
  const built = platforms.map((platform) => `stanza-${platform}`);

  expect(built.length).toBeGreaterThan(0);
  expect(assets.toSorted()).toEqual(built);
});

test("the README explain row takes the location the usage takes and names all three kinds", () => {
  const explain = tableAfter(readme, ["Command", "What it does"]).find(([command]) =>
    command.startsWith("stanza explain "),
  );

  expect(explain).toBeDefined();

  const [command, summary] = explain!;
  const usage = /^\s*stanza explain .*?(\S+)$/m.exec(catalog.usage)?.[1];

  expect(command.slice("stanza explain ".length)).toBe(usage!);

  for (const kind of [/\bgap\b/, /\bbraced body\b/, /\bwall\b/]) expect(summary).toMatch(kind);
});

test("every README link into the site names a page in the site map", () => {
  const links = [...readme.matchAll(/https:\/\/stanza\.ryuu\.gg(\/[^)"\s]*)?/g)].map(
    ([, path]) => path ?? "/",
  );

  expect(links.length).toBeGreaterThan(0);
  expect(links.filter((path) => !pages.includes(path))).toEqual([]);
});
