import { expect, test } from "bun:test";
import { cpSync, mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { platforms } from "../scripts/platform.ts";
import { texts } from "./docs-pages.ts";
import { tableAfter } from "./rule-tables.ts";
import { run, scratch } from "./support.ts";

const root = join(import.meta.dir, "..");

function processed(url: string): string {
  const text = texts[url];
  expect(text, `${url} has no page text`).toBeDefined();
  return text!.processed;
}

function fence(text: string, title: string): string {
  const lines = text.split("\n");
  const opening = lines.findIndex(
    (line) => line.startsWith("```") && line.includes(`title="${title}"`),
  );

  expect(opening, `no code block titled ${title}`).toBeGreaterThanOrEqual(0);

  const closing = lines.findIndex((line, index) => index > opening && line === "```");
  return lines.slice(opening + 1, closing).join("\n");
}

test("the release binaries page lists exactly the built assets", () => {
  const table = tableAfter(processed("/guides/release-binaries"), ["asset", "platform"]);
  const assets = table.map(([asset]) => asset);
  const built = platforms.map((platform) => `stanza-${platform}`);

  expect(built.length).toBeGreaterThan(0);
  expect(assets.toSorted()).toEqual(built);
});

test("the getting started page prints what the launcher prints without Bun", () => {
  const launched = Bun.spawnSync(["/bin/sh", join(root, "src", "launcher"), "--version"], {
    env: { PATH: scratch("no-bun") },
  });

  expect(launched.exitCode).toBe(2);
  expect(fence(processed("/getting-started"), "Without Bun")).toBe(
    launched.stderr.toString().trimEnd(),
  );
});

test("the getting started page walks through what check and fix do", () => {
  const cwd = scratch("getting-started");
  const example = join(root, "tests", "fixtures", "after-guard");
  const file = join(cwd, "src", "publish.ts");
  mkdirSync(join(cwd, "src"));
  cpSync(join(example, "example.before.ts"), file);

  const checked = run({ cwd }, "--check", "src");
  expect(checked.code).toBe(1);
  expect(fence(processed("/getting-started"), "Output")).toBe(checked.stdout.trimEnd());

  const fixed = run({ cwd }, "--fix", "src");
  expect(fixed.code).toBe(0);
  expect(readFileSync(file, "utf8")).toBe(readFileSync(join(example, "example.after.ts"), "utf8"));

  const rechecked = run({ cwd }, "--check", "src");
  expect(rechecked.code).toBe(0);
  expect(rechecked.stdout).toBe("");
});
