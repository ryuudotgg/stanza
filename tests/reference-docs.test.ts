import { expect, test } from "bun:test";
import { chmodSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import catalog from "../docs/lib/catalog.json";
import { pages, texts } from "./docs-pages.ts";
import { row, tableAfter } from "./rule-tables.ts";
import { batch, run, scratch, stanzaBinary, superproject } from "./support.ts";

const { flags, usage, extensions } = catalog;
const docsRoot = join(import.meta.dir, "..", "docs", "content", "docs");
const examplePath = "src/load.ts";

function page(url: string): { raw: string; processed: string; path: string } {
  const text = texts[url];
  expect(text, `${url} has no page text`).toBeDefined();
  return text!;
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

function example(url: string): Buffer {
  const { raw, path } = page(url);
  const included = /<include\b[^>]*title="src\/load\.ts"[^>]*>\s*([^<]*?)\s*<\/include>/.exec(raw);
  expect(included, `${url} includes no ${examplePath} example`).not.toBeNull();

  return readFileSync(resolve(docsRoot, dirname(path), included![1]!));
}

test("the CLI page lists every flag the CLI has", () => {
  const { processed } = page("/reference/cli");
  expect<string[][]>(tableAfter(processed, ["flag", "what it does"])).toEqual(flags);

  expect(processed).toContain(`\`\`\`text\n${usage}\n\`\`\``);

  for (const [flag] of flags) {
    const line = processed
      .split("\n")
      .find((line) => line.trim().startsWith("|") && row(line)[0] === flag);

    expect(line, `${flag} has no table row`).toBeDefined();

    const href = /\[`[^`]+`\]\(([^)]+)\)/.exec(line!.split("|")[1]!)?.[1];
    if (flag === "--help" || flag === "--version") {
      expect(href).toBeUndefined();
      continue;
    }

    expect(href, `${flag} has no link`).toBeDefined();
    const [path, hash] = href!.split("#");
    expect(pages, `${flag} links to a missing page`).toContain(path!);
    if (hash) expect(texts[path!]!.processed).toContain(`[#${hash}]`);
  }
});

test("the File Selection page lists the extensions that parse JSX", () => {
  const answers = batch(
    scratch("jsx"),
    extensions.map((extension) => ({
      path: `file${extension}`,
      text: "<div />\n",
      mode: "check" as const,
      braces: null,
      changedLines: null,
      explain: [],
    })),
  );

  for (const answer of answers) expect(answer.error).toBeNull();

  const parsing = extensions.filter(
    (_, index) => !answers[index]!.findings.some((finding) => finding.rule === "parse"),
  );

  const { raw } = page("/reference/file-selection");
  const lines = raw.split("\n");
  const heading = lines.indexOf("## JSX");
  expect(heading).toBeGreaterThanOrEqual(0);

  const nextHeading = lines.findIndex((line, index) => index > heading && line.startsWith("## "));
  const section = lines.slice(heading, nextHeading === -1 ? undefined : nextHeading).join("\n");
  const paragraph = section
    .split(/\n\s*\n/)
    .slice(1)
    .find((text) => text.trim() !== "");

  expect(paragraph).toBeDefined();

  const listed = [...paragraph!.matchAll(/`(\.[a-z]+)`/g)].map((match) => match[1]!);
  expect(listed.sort()).toEqual(parsing.sort());
  expect(section).toContain('<DiffExample fixture="jsx/example.tsx" />');
});

test("the output page prints the finding stanza prints", () => {
  const url = "/reference/output";
  const stdin = example(url);
  const cwd = scratch("reference");

  const checked = run({ cwd, stdin }, "--check", "--stdin", examplePath);
  expect(checked.code).toBe(1);
  expect(fence(page(url).processed, "Finding")).toBe(checked.stdout.trimEnd());

  const json = run({ cwd, stdin }, "--check", "--json", "--stdin", examplePath);
  expect(JSON.parse(fence(page(url).processed, "--json"))).toEqual(JSON.parse(json.stdout));
});

test("the explain page prints what explain prints", () => {
  const url = "/reference/explain";
  const cwd = scratch("reference");
  mkdirSync(join(cwd, dirname(examplePath)), { recursive: true });
  writeFileSync(join(cwd, examplePath), example(url));

  const explained = run({ cwd }, "explain", `${examplePath}:5`);
  expect(explained.code).toBe(0);
  expect(fence(page(url).processed, "Output")).toBe(explained.stdout.trimEnd());
  expect(page(url).processed).toContain(`\`stanza explain ${examplePath}:5\``);
});

function stderrFence(url: string, title: string, result: ReturnType<typeof run>, code: number) {
  expect(result.stdout).toBe("");
  expect(result.code).toBe(code);
  expect(`${fence(page(url).processed, title)}\n`).toBe(result.stderr);
}

test("the File Selection page prints the lines stanza prints for a submodule and a FIFO", () => {
  const url = "/reference/file-selection";
  stderrFence(url, "Skipped Submodule", run({ cwd: superproject({}) }, "--check", "."), 0);

  const cwd = scratch("reference");
  expect(Bun.spawnSync(["mkfifo", join(cwd, "pipe.ts")]).exitCode).toBe(0);
  stderrFence(url, "Not a Regular File", run({ cwd }, "--check", "pipe.ts"), 2);
});

test.skipIf(process.getuid?.() === 0)(
  "the File Selection page prints the line stanza prints for a folder it cannot open",
  () => {
    const cwd = scratch("reference");
    const locked = join(cwd, "locked");
    mkdirSync(locked);
    writeFileSync(join(locked, "a.ts"), "export const a = 1;\n");
    chmodSync(locked, 0o000);

    try {
      const result = run({ cwd }, "--check", "locked");
      stderrFence("/reference/file-selection", "Unreadable Folder", result, 2);
    } finally {
      chmodSync(locked, 0o755);
    }
  },
);

test("the output page prints the line stanza prints from a deleted working directory", () => {
  const gone = join(scratch("reference"), "gone");
  mkdirSync(gone);
  const env = Object.fromEntries(
    Object.entries(process.env).filter(([key]) => key !== "FORCE_COLOR"),
  );

  const script = 'cd "$1" && rmdir "$1" && shift && exec "$@"';
  const command = ["sh", "-c", script, "sh", gone, stanzaBinary(), "--check", examplePath];
  const spawned = Bun.spawnSync(command, { env });

  const result = {
    code: spawned.exitCode,
    stdout: spawned.stdout.toString(),
    stderr: spawned.stderr.toString(),
  };

  stderrFence("/reference/output", "Deleted Working Directory", result, 2);
});
