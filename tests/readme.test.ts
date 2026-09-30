import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { diffLines } from "../docs/lib/diff.ts";
import { claudeCodeHooks } from "../src/hook.ts";
import { platforms } from "../scripts/platform.ts";
import { pages } from "./docs-pages.ts";
import { tableAfter } from "./rule-tables.ts";

const root = join(import.meta.dir, "..");
const readme = readFileSync(join(root, "README.md"), "utf8");
const lines = readme.split("\n");

test("the README prints the Claude Code hook registration", () => {
  const [block] = blocksAfter(
    "To format each Write before it lands, fix each Edit and MultiEdit as soon as it lands, and run a Stop pass at the end of each turn in Claude Code, add the three hooks to `.claude/settings.json`:",
    1,
  );

  expect(block?.opening).toBe("```json");
  expect(JSON.parse(block!.body)).toEqual(JSON.parse(claudeCodeHooks));

  const command = [{ type: "command", command: "stanza hook" }];
  expect(JSON.parse(claudeCodeHooks)).toEqual({
    hooks: {
      PreToolUse: [{ matcher: "Write", hooks: command }],
      PostToolUse: [{ matcher: "Edit|MultiEdit", hooks: command }],
      Stop: [{ hooks: command }],
    },
  });
});

function blocksAfter(marker: string, count: number): { opening: string; body: string }[] {
  let cursor = lines.indexOf(marker);
  expect(cursor, `${marker} is missing`).toBeGreaterThanOrEqual(0);

  return Array.from({ length: count }, () => {
    const opening = lines.findIndex((line, index) => index > cursor && line.startsWith("```"));
    const closing = lines.findIndex((line, index) => index > opening && line === "```");
    expect(closing).toBeGreaterThan(opening);

    cursor = closing;
    return { opening: lines[opening]!, body: `${lines.slice(opening + 1, closing).join("\n")}\n` };
  });
}

test("the README example is the after-guard docs example", () => {
  const fixture = (name: string) =>
    readFileSync(join(root, "tests", "fixtures", "after-guard", name), "utf8");

  const [block] = blocksAfter("Before and after `stanza --fix`:", 1);
  const rendered = diffLines(fixture("example.before.ts"), fixture("example.after.ts"))
    .map(({ kind, text }) => `${kind === "add" ? "+" : kind === "remove" ? "-" : " "}${text}`)
    .join("\n");

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
