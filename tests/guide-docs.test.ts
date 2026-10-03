import { expect, test } from "bun:test";
import { cpSync, mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { platforms } from "../scripts/platform.ts";
import { archiveName, expectedAssetNames } from "../scripts/release.ts";
import { claudeCodeHooks } from "../src/hook.ts";
import { texts } from "./docs-pages.ts";
import { everyTableWithHeader } from "./rule-tables.ts";
import { run, scratch, scratchGitRepository } from "./support.ts";

const root = join(import.meta.dir, "..");

function processed(url: string): string {
  const text = texts[url];
  expect(text, `${url} has no page text`).toBeDefined();
  return text!.processed;
}

function fence(text: string, title: string): string {
  const lines = text.split("\n");
  const opening = lines.findIndex(
    (line) => /^\s*```/.test(line) && line.includes(`title="${title}"`),
  );

  expect(opening, `no code block titled ${title}`).toBeGreaterThanOrEqual(0);

  const indentation = /^\s*/.exec(lines[opening]!)![0];
  const closing = lines.findIndex(
    (line, index) => index > opening && line === `${indentation}\`\`\``,
  );

  expect(closing, `no closing fence for ${title}`).toBeGreaterThan(opening);

  return lines
    .slice(opening + 1, closing)
    .map((line) => (line.startsWith(indentation) ? line.slice(indentation.length) : line))
    .join("\n");
}

const binaryTabs = [
  { tab: "macOS", platform: /^darwin-/, checksum: "shasum -a 256 -c" },
  { tab: "Linux (glibc)", platform: /^linux-(x64|arm64)$/, checksum: "sha256sum -c" },
  { tab: "Linux (musl)", platform: /^linux-.*-musl$/, checksum: "sha256sum -c" },
];

function tabBody(text: string, value: string): string {
  const start = text.indexOf(`<Tab value="${value}">`);
  expect(start, `no ${value} tab`).toBeGreaterThanOrEqual(0);
  return text.slice(start, text.indexOf("</Tab>", start));
}

test("each release binaries tab installs the built assets for its platform", () => {
  const text = processed("/guides/release-binaries");
  const built = platforms.map((platform) => `stanza-${platform}`);
  expect(built.length).toBeGreaterThan(0);

  const listed = binaryTabs.flatMap(({ tab, platform, checksum }) => {
    const body = tabBody(text, tab);
    const assets = everyTableWithHeader(body, ["asset", "machine"]).map(([asset]) => asset);
    const own = platforms.filter((name) => platform.test(name)).map((name) => `stanza-${name}`);

    expect(assets.toSorted(), `${tab} assets`).toEqual(own);
    expect(/^[ \t]*asset=(\S+)/m.exec(body)?.[1], `${tab} asset=`).toBe(assets[0]);

    expect(body, `${tab} checksum`).toContain(`| ${checksum} &&`);
    expect(body, `${tab} download`).toContain("/download/$asset.tar.xz");
    expect(body, `${tab} archive checksum`).toContain('grep " $asset.tar.xz\\$" SHA256SUMS');
    expect(body, `${tab} extraction`).toContain("tar -xJf $asset.tar.xz");

    for (const asset of assets)
      expect(expectedAssetNames()).toContain(archiveName(asset.slice("stanza-".length)));

    return assets;
  });

  expect(listed.toSorted()).toEqual(built);
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

const codexHooks = {
  hooks: {
    PostToolUse: [{ matcher: "apply_patch", hooks: [{ type: "command", command: "stanza hook" }] }],
    Stop: [{ hooks: [{ type: "command", command: "stanza hook" }] }],
  },
};

function withHunks(registration: unknown): unknown {
  return JSON.parse(
    JSON.stringify(registration).replaceAll('"stanza hook"', '"stanza hook --hunks"'),
  );
}

function streams(text: string): [string, string, string][] {
  const lines = text.split("\n");
  const header = lines.findIndex((line) =>
    /^\|\s*mode\s*\|\s*stdout\s*\|\s*stderr\s*\|$/.test(line),
  );

  expect(header, "mode | stdout | stderr table is missing").toBeGreaterThanOrEqual(0);

  const end = lines.findIndex((line, index) => index > header + 1 && !line.startsWith("|"));
  return lines.slice(header + 2, end < 0 ? undefined : end).map((line) => {
    const [mode = "", stdout = "", stderr = ""] = line
      .split("|")
      .slice(1, -1)
      .map((cell) => cell.trim());

    return [mode, stdout, stderr];
  });
}

test("the Claude Code page registers every hook as claudeCodeHooks does", () => {
  const registration = JSON.parse(fence(processed("/agents/claude-code"), ".claude/settings.json"));
  expect(registration).toEqual(JSON.parse(claudeCodeHooks));
});

test("the Codex page registers the PostToolUse and Stop hooks", () => {
  expect(JSON.parse(fence(processed("/agents/codex"), ".codex/hooks.json"))).toEqual(codexHooks);
  expect(JSON.parse(fence(processed("/agents/codex"), "~/.codex/hooks.json"))).toEqual(codexHooks);
});

test("the existing codebases page adds --hunks to every hook and nothing else", () => {
  const text = processed("/guides/existing-codebases");

  expect(JSON.parse(fence(text, ".claude/settings.json"))).toEqual(
    withHunks(JSON.parse(claudeCodeHooks)),
  );

  expect(JSON.parse(fence(text, ".codex/hooks.json"))).toEqual(withHunks(codexHooks));
  expect(fence(text, "pre-commit")).toBe(
    `${fence(processed("/guides/pre-commit"), "pre-commit")} --hunks`,
  );
});

test("the pre-commit page prints the fix command a blocked commit prints", () => {
  const source = readFileSync(
    join(root, "tests", "fixtures", "after-guard", "example.before.ts"),
    "utf8",
  );

  const cwd = scratchGitRepository({ files: { "src/load.ts": source }, staged: true });
  const text = processed("/guides/pre-commit");

  const hook = fence(text, "pre-commit");
  expect(hook).toBe("#!/bin/sh\nexec stanza --check --staged");
  expect(text).toContain(`printf '${hook.replaceAll("\n", "\\n")}\\n'`);

  const checked = run({ cwd }, "--check", "--staged");
  expect(checked.code).toBe(1);

  const last = checked.stderr.trimEnd().split("\n").at(-1)!;
  expect(fence(text, "Fix command")).toBe(last.replace(cwd, "/path/to/repo"));
});

const wall = `export function f() {\n${"  step();\n".repeat(6)}}\n`;
const unfixed = Buffer.from(wall.replace("{\n", "{\n\n"));
const findings = /^wall\.ts:\d+:\d+ \S+ /;

test("the output page states the stream each stdin mode writes to", () => {
  const cwd = scratch("editors");

  const fixed = run({ cwd, stdin: unfixed }, "--fix", "--stdin", "wall.ts");
  expect(fixed.stdout).toBe(wall);
  expect(fixed.stderr).toMatch(findings);

  const checked = run({ cwd, stdin: unfixed }, "--check", "--stdin", "wall.ts");
  expect(checked.stdout).toMatch(findings);
  expect(checked.stderr).toBe("");

  expect(streams(processed("/reference/output"))).toEqual([
    ["`--fix --stdin`", "the fixed text", "findings"],
    ["`--check --stdin`", "findings", "warnings only"],
  ]);
});

test("the editors page accepts the exit code --fix --stdin gives for findings it cannot fix", () => {
  const fixed = run({ cwd: scratch("editors"), stdin: unfixed }, "--fix", "--stdin", "wall.ts");
  expect(fixed.code).toBe(1);
  expect(fixed.stdout).toBe(wall);

  const conform = fence(processed("/guides/editors"), "conform.lua");
  expect(conform).toContain('args = { "--fix", "--stdin", "$FILENAME" }');
  expect(conform).toContain("exit_codes = { 0, 1 }");
});
