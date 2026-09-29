import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { claudeCodeHooks } from "../src/hook.ts";
import { platforms } from "../scripts/platform.ts";
import { pages } from "./docs-pages.ts";
import { tableAfter } from "./rule-tables.ts";

const root = join(import.meta.dir, "..");
const readme = readFileSync(join(root, "README.md"), "utf8");
const lines = readme.split("\n");

test("the README prints the Claude Code hook registration", () => {
  expect(readme).toContain(claudeCodeHooks);

  const command = [{ type: "command", command: "stanza hook" }];
  expect(JSON.parse(claudeCodeHooks)).toEqual({
    hooks: { PreToolUse: [{ matcher: "Write", hooks: command }], Stop: [{ hooks: command }] },
  });
});

function blocksAfter(marker: string, count: number): string[] {
  let cursor = lines.indexOf(marker);
  expect(cursor, `${marker} is missing`).toBeGreaterThanOrEqual(0);

  return Array.from({ length: count }, () => {
    const opening = lines.findIndex((line, index) => index > cursor && line.startsWith("```"));
    const closing = lines.findIndex((line, index) => index > opening && line === "```");
    expect(closing).toBeGreaterThan(opening);

    cursor = closing;
    return `${lines.slice(opening + 1, closing).join("\n")}\n`;
  });
}

test("the README example is the after-guard docs example", () => {
  const fixture = (name: string) =>
    readFileSync(join(root, "tests", "fixtures", "after-guard", name), "utf8");

  expect(blocksAfter("Before and after `stanza --fix`:", 2)).toEqual([
    fixture("example.before.ts"),
    fixture("example.after.ts"),
  ]);
});

test("the README release binaries table lists exactly the built assets", () => {
  const assets = tableAfter(readme, ["Asset", "Platform"]).map(([asset]) => asset);
  const built = platforms.map((platform) => `stanza-${platform}`);

  expect(built.length).toBeGreaterThan(0);
  expect(assets.toSorted()).toEqual(built);
});

test("every README link into the site names a page in the site map", () => {
  const links = [...readme.matchAll(/\(https:\/\/stanza\.ryuu\.gg(\/[^)]*)?\)/g)].map(
    ([, path]) => path ?? "/",
  );

  expect(links.length).toBeGreaterThan(0);
  expect(links.filter((path) => !pages.includes(path))).toEqual([]);
});
