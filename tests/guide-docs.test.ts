import { expect, test } from "bun:test";
import { cpSync, mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { format } from "prettier";
import catalog from "../docs/lib/catalog.json";
import { hookSettings, printJson } from "../docs/lib/hooks.ts";
import { platforms } from "../scripts/packages.ts";
import { archiveName, expectedAssetNames } from "../scripts/release.ts";
import { everyTableWithHeader, fence, page } from "./docs-support.ts";
import { run, scratch, scratchGitRepository, unfixed, wall } from "./support.ts";

const root = join(import.meta.dir, "..");

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
  const text = page("/guides/release-binaries").processed;
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

test("the getting started page walks through what check and fix do", () => {
  const cwd = scratch("getting-started");
  const example = join(root, "tests", "fixtures", "after-guard");
  const file = join(cwd, "src", "publish.ts");
  mkdirSync(join(cwd, "src"));
  cpSync(join(example, "example.before.ts"), file);

  const text = page("/getting-started").processed;

  const checked = run({ cwd }, "--check", "src");
  expect(checked.code).toBe(1);
  expect(fence(text, "Output")).toBe(checked.stdout.trimEnd());
  expect(fence(text, "Summary")).toBe(checked.stderr.trimEnd());

  const fixed = run({ cwd }, "--fix", "src");
  expect(fixed.code).toBe(0);
  expect(readFileSync(file, "utf8")).toBe(readFileSync(join(example, "example.after.ts"), "utf8"));
  expect(fence(text, "Fix Summary")).toBe(fixed.stderr.trimEnd());

  const rechecked = run({ cwd }, "--check", "src");
  expect(rechecked.code).toBe(0);
  expect(rechecked.stdout).toBe("");
  expect(fence(text, "Clean Summary")).toBe(rechecked.stderr.trimEnd());
});

function registrationFence(url: string, title: string, hunks = false): string {
  const text = fence(page(url).processed, title);
  expect(text).toBe(printJson(hookSettings(title, hunks)));
  return text;
}

test("the Claude Code page renders its registration from the catalog", () => {
  registrationFence("/agents/claude-code", ".claude/settings.json");
});

test("the Codex page renders the registration for both hooks.json files", () => {
  const { files } = catalog.registrations.Codex;
  expect(files).toEqual(["~/.codex/hooks.json", ".codex/hooks.json"]);
  for (const title of files) registrationFence("/agents/codex", title);
});

test("the existing codebases page adds --hunks to every hook in one tab group", () => {
  const url = "/guides/existing-codebases";
  const text = page(url).processed;

  for (const [title, client] of [
    [".claude/settings.json", "Claude Code"],
    [".codex/hooks.json", "Codex"],
  ] as const) {
    const commands: string[] = [];
    JSON.parse(registrationFence(url, title, true), (key, value) => {
      if (key === "command") commands.push(value);
      return value;
    });

    expect(new Set(commands), client).toEqual(new Set(["stanza hook --hunks"]));
  }

  expect(text.match(/<CodeBlockTabs\b/g), "one tab group").toHaveLength(1);
  expect([...text.matchAll(/<CodeBlockTab value="([^"]+)">/g)].map(([, value]) => value)).toEqual([
    "Claude Code",
    "Codex",
    "Pre-commit",
  ]);

  expect(fence(text, "pre-commit")).toBe(
    `${fence(page("/guides/pre-commit").processed, "pre-commit")} --hunks`,
  );
});

test("each rendered registration is laid out as Prettier lays out JSON at width 100", async () => {
  for (const { files } of Object.values(catalog.registrations))
    for (const hunks of [false, true]) {
      const settings = hookSettings(files[0]!, hunks);
      const text = printJson(settings);
      const prettier = await format(JSON.stringify(settings), { parser: "json", printWidth: 100 });
      expect(prettier).toBe(`${text}\n`);
      expect(JSON.parse(text)).toEqual(settings);
    }
});

test("the pre-commit page prints the fix command a blocked commit prints", () => {
  const source = readFileSync(
    join(root, "tests", "fixtures", "after-guard", "example.before.ts"),
    "utf8",
  );

  const cwd = scratchGitRepository({ files: { "src/load.ts": source }, staged: true });
  const text = page("/guides/pre-commit").processed;

  const hook = fence(text, "pre-commit");
  expect(hook).toBe("#!/bin/sh\nexec stanza --check --staged");
  expect(text).toContain(`printf '${hook.replaceAll("\n", "\\n")}\\n'`);

  const checked = run({ cwd }, "--check", "--staged");
  expect(checked.code).toBe(1);

  const [summary, last] = checked.stderr.trimEnd().split("\n").slice(-2);
  expect(fence(text, "Summary")).toBe(summary!);
  expect(fence(text, "Fix command")).toBe(last!.replace(cwd, "/path/to/repo"));
});

test("the editors page accepts the exit code --fix --stdin gives for findings it cannot fix", () => {
  const fixed = run({ cwd: scratch("editors"), stdin: unfixed }, "--fix", "--stdin", "wall.ts");
  expect(fixed.code).toBe(1);
  expect(fixed.stdout).toBe(wall);

  const conform = fence(page("/guides/editors").processed, "conform.lua");
  expect(conform).toContain('args = { "--fix", "--stdin", "$FILENAME" }');
  expect(conform).toContain("exit_codes = { 0, 1 }");
});
