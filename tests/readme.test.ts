import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { claudeCodeHooks } from "../src/hook.ts";
import { flags } from "../src/usage.ts";
import { platforms } from "../scripts/platform.ts";
import { run, scratch } from "./support.ts";
import { catalog, row } from "./rule-tables.ts";

const readme = readFileSync(join(import.meta.dir, "..", "README.md"), "utf8");
const lines = readme.split("\n");

test("the README prints the Claude Code hook registration", () => {
  expect(readme).toContain(claudeCodeHooks);

  const command = [{ type: "command", command: "stanza hook" }];
  expect(JSON.parse(claudeCodeHooks)).toEqual({
    hooks: { PreToolUse: [{ matcher: "Write", hooks: command }], Stop: [{ hooks: command }] },
  });
});

function blockAfter(marker: string): string {
  const start = lines.findIndex((line) => line.includes(marker));
  expect(start).toBeGreaterThanOrEqual(0);

  const opening = lines.findIndex((line, index) => index > start && line.startsWith("```"));
  const closing = lines.findIndex((line, index) => index > opening && line === "```");
  expect(closing).toBeGreaterThan(opening);

  return lines.slice(opening + 1, closing).join("\n");
}

test("the README prints the Codex Stop hook registration", () => {
  const registration = JSON.parse(blockAfter("`.codex/hooks.json`"));

  expect(registration).toEqual({
    hooks: { Stop: [{ hooks: [{ type: "command", command: "stanza hook" }] }] },
  });

  expect(readme).toContain("trust it with `/hooks`");
});

test("the README names the stream stdin findings go to in each mode", () => {
  expect(readme).toContain(
    "`--fix --stdin` prints the fixed text on stdout and findings on stderr, so stdout holds only code. `--check --stdin` prints findings on stdout",
  );

  const cwd = scratch("readme");
  const stdin = Buffer.from(`export function f() {\n${"  step();\n".repeat(6)}}\n`);

  const fixed = run({ cwd, stdin }, "--fix", "--stdin", "wall.ts");
  expect(fixed.stdout).toBe(stdin.toString());
  expect(fixed.stderr).toStartWith("wall.ts:2:3 wall ");

  const checked = run({ cwd, stdin }, "--check", "--stdin", "wall.ts");
  expect(checked.stdout).toStartWith("wall.ts:2:3 wall ");
  expect(checked.stderr).toBe("");
});

test("the README Use list matches the CLI flag table", () => {
  const start = lines.indexOf("## Use");
  expect(start).toBeGreaterThanOrEqual(0);

  const opening = lines.findIndex((line, index) => index > start && line.startsWith("```"));
  expect(opening).toBeGreaterThan(start);

  const closing = lines.findIndex((line, index) => index > opening && line === "```");
  expect(closing).toBeGreaterThan(opening);

  const rows = lines.slice(opening + 1, closing).map((line) => line.split(/\s{2,}/));
  expect(rows).toEqual(flags.map((flag) => [...flag]));
});

function tableAfter(heading: string): [string, string][] {
  const start = lines.indexOf(heading);
  expect(start).toBeGreaterThanOrEqual(0);

  const header = lines.findIndex((line, index) => index > start && line.startsWith("|"));
  const end = lines.findIndex((line, index) => index > header && !line.startsWith("|"));
  expect(lines[header + 1]).toMatch(/^\|[\s|-]+\|$/);

  return lines.slice(header + 2, end).map(row);
}

test("the --fix table matches the fixable rules", () => {
  expect(tableAfter("Applied by `--fix`:")).toEqual(catalog(true));
});

test("the --check table matches the report only rules", () => {
  expect(tableAfter("Reported by `--check`, never fixed:")).toEqual(catalog(false));
});

test("the README release binaries table lists exactly the built assets", () => {
  const assets = tableAfter("### Release binaries").map(([asset]) => asset);
  const built = platforms.map((platform) => `stanza-${platform}`);

  expect(built.length).toBeGreaterThan(0);
  expect(assets.toSorted()).toEqual(built);
});
