import { expect, test } from "bun:test";
import { chmodSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import catalog from "../docs/lib/catalog.json";
import { pages } from "./docs-pages.ts";
import { fence, page, rawTableAfter, row, section, tableAfter } from "./docs-support.ts";
import {
  batch,
  colorless,
  findingRow,
  gitIn,
  run,
  scratch,
  scratchGitRepository,
  stanzaBinary,
  superproject,
  unfixed,
  wall,
} from "./support.ts";

const { flags, usage, extensions } = catalog;
const docsRoot = join(import.meta.dir, "..", "docs", "content", "docs");
const examplePath = "src/load.ts";
function example(url: string, title = examplePath): Buffer {
  const { raw, path } = page(url);
  const included = new RegExp(
    `<include\\b[^>]*title="${title.replaceAll(".", "\\.")}"[^>]*>\\s*([^<]*?)\\s*<\\/include>`,
  ).exec(raw);

  expect(included, `${url} includes no ${title} example`).not.toBeNull();

  return readFileSync(resolve(docsRoot, dirname(path), included![1]!));
}

test("the CLI page renders the catalog's usage and flag rows", () => {
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
    if (hash) expect(page(path!).processed).toContain(`[#${hash}]`);
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
  const jsx = lines.slice(heading, nextHeading === -1 ? undefined : nextHeading).join("\n");
  const paragraph = jsx
    .split(/\n\s*\n/)
    .slice(1)
    .find((text) => text.trim() !== "");

  expect(paragraph).toBeDefined();

  const listed = [...paragraph!.matchAll(/`(\.[a-z]+)`/g)].map((match) => match[1]!);
  expect(listed.sort()).toEqual(parsing.sort());
  expect(jsx).toContain('<DiffExample fixture="jsx/example.tsx" />');
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

test("the output page prints the summary a check of the example ends with", () => {
  const url = "/reference/output";
  const cwd = scratch("reference");
  mkdirSync(join(cwd, "src"));
  writeFileSync(join(cwd, examplePath), example(url));

  const checked = run({ cwd }, "--check", examplePath);
  expect(checked.code).toBe(1);
  expect(fence(page(url).processed, "Summary")).toBe(checked.stderr.trimEnd());
});

test("every finding the output page shows reads as a finding row", () => {
  const { raw } = page("/reference/output");
  expect(fence(raw, "Finding")).toMatch(findingRow);
  expect(raw).toContain("```text\npath:line:col: rule: message\n```");

  const rows = raw.split("\n").filter((line) => line.startsWith(`${examplePath}:`));
  expect(rows.length).toBeGreaterThanOrEqual(5);
  for (const line of rows) expect(line).toMatch(findingRow);
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

test("the explain page prints the wall explain prints", () => {
  const url = "/reference/explain";
  const cwd = scratch("reference-wall");
  mkdirSync(join(cwd, "src"), { recursive: true });
  writeFileSync(join(cwd, "src/boot.ts"), example(url, "src/boot.ts"));

  const explained = run({ cwd }, "explain", "src/boot.ts:2");
  expect(explained.code).toBe(0);
  expect(fence(page(url).processed, "Wall Output")).toBe(explained.stdout.trimEnd());
  expect(page(url).processed).toContain("`stanza explain src/boot.ts:2`");
  const exits = section(url, "Exit Status").split("\n");
  expect(exits.filter((line) => line.startsWith("| 1 ")).map(row)).toEqual([
    ["1", "no gap ends, no braced body starts and no wall runs through that line"],
  ]);
});

test("the hook protocol page prints the reason the Stop hook gives", () => {
  const fixtures = join(import.meta.dir, "fixtures", "hook", "reason");
  const files = Object.fromEntries(
    readdirSync(fixtures).map((name) => [
      `src/${name}`,
      readFileSync(join(fixtures, name), "utf8"),
    ]),
  );

  const cwd = scratchGitRepository({ files, staged: true });
  const env = { ...process.env, AGENT_HOOKS: "1" };
  const result = run({ cwd, env, stdin: Buffer.from(JSON.stringify({ cwd })) }, "hook");

  expect(result.code).toBe(0);
  expect(fence(page("/reference/hook-protocol").processed, "Reason")).toBe(
    JSON.parse(result.stdout).reason,
  );
});

function stderrFence(url: string, title: string, result: ReturnType<typeof run>, code: number) {
  expect(result.stdout).toBe("");
  expect(result.code).toBe(code);
  expect(`${fence(page(url).processed, title)}\n`).toBe(result.stderr);
}

test("the CLI page prints the lines stanza prints for a usage error", () => {
  stderrFence("/reference/cli", "Usage Error", run("--chek", "src"), 2);
});

test("the File Selection page prints the lines stanza prints for a submodule and a FIFO", () => {
  const url = "/reference/file-selection";
  stderrFence(url, "Skipped Submodule", run({ cwd: superproject({}) }, "--check", "."), 0);

  const cwd = scratch("reference");
  expect(Bun.spawnSync(["mkfifo", join(cwd, "pipe.ts")]).exitCode).toBe(0);
  stderrFence(url, "Not a Regular File", run({ cwd }, "--check", "pipe.ts"), 2);
});

const braced = "if (a) {\n  b();\n}\n";

function findingsOf(stdout: string): [string, string][] {
  return JSON.parse(stdout).map((finding: { path: string; rule: string }) => [
    finding.path,
    finding.rule,
  ]);
}

test("the File Selection page's skipped names stay unselected below the folder passed", () => {
  const cwd = scratch("reference-skipped");
  const skipped = [
    "a.d.mts",
    "a.d.cts",
    ".next/a.ts",
    "out/a.ts",
    "coverage/a.ts",
    "migrations/a.ts",
    "drizzle/a.ts",
  ];

  for (const path of [...skipped, "keep.ts"]) {
    mkdirSync(dirname(join(cwd, path)), { recursive: true });
    writeFileSync(join(cwd, path), braced);
  }

  const checked = run({ cwd }, "--check", "--json", ".");
  expect(checked.stderr).toBe("");
  expect(checked.code).toBe(1);
  expect(findingsOf(checked.stdout)).toEqual([["keep.ts", "braces"]]);
});

test("the File Selection page's unsupported path is an error, from disk and on stdin", () => {
  const cwd = scratch("reference-unsupported");
  const source = "export {};\n";
  writeFileSync(join(cwd, "x.md"), source);
  writeFileSync(join(cwd, "x.ts"), source);
  expect(run({ cwd }, "--check", "x.ts").code).toBe(0);

  const stdin = Buffer.from(source);
  for (const result of [
    run({ cwd }, "--check", "x.md"),
    run({ cwd, stdin }, "--check", "--stdin", "x.md"),
  ]) {
    expect(result.code).toBe(2);
    expect(result.stdout).toBe("");
    expect(result.stderr).toContain("x.md");
  }
});

test("the File Selection page takes a named file though .gitignore lists it", () => {
  const cwd = scratchGitRepository({
    files: { ".gitignore": "ignored.ts\n", "ignored.ts": braced },
  });

  const folder = run({ cwd }, "--check", "--json", ".");
  expect(folder.code).toBe(0);
  expect(JSON.parse(folder.stdout)).toEqual([]);

  const named = run({ cwd }, "--check", "--json", "ignored.ts");
  expect(named.code).toBe(1);
  expect(findingsOf(named.stdout)).toEqual([["ignored.ts", "braces"]]);
});

test("the File Selection page's --changed covers the whole repository from a subfolder", () => {
  const cwd = scratchGitRepository({ files: { "root.ts": "b();\n", "sub/a.ts": "b();\n" } });
  gitIn(cwd, "add", ".");
  gitIn(cwd, "commit", "-q", "-m", "base");
  writeFileSync(join(cwd, "root.ts"), braced);
  writeFileSync(join(cwd, "sub", "a.ts"), braced);

  const changed = run({ cwd: join(cwd, "sub") }, "--check", "--json", "--changed");
  expect(changed.code).toBe(1);

  const paths = findingsOf(changed.stdout).map(([path]) => path);
  expect([...new Set(paths)].sort()).toEqual(["../root.ts", "a.ts"]);
});

test("the CLI page's -h prints what --help prints", () => {
  const short = run("-h");
  expect(short).toEqual(run("--help"));
  expect(short.code).toBe(0);
  expect(short.stderr).toBe("");
  expect(short.stdout).not.toBe("");
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
  const env = colorless();

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

test("the output reference quotes the width warning and its fallback", () => {
  const cwd = scratch("reference-width");
  writeFileSync(join(cwd, ".oxfmtrc.json"), "{");
  writeFileSync(join(cwd, ".prettierrc.json"), '{ "printWidth": 40 }');
  writeFileSync(join(cwd, "a.ts"), "const value = 1;\n");

  const checked = run({ cwd }, "--check", "a.ts");
  const warnings = section("/reference/output", "Warnings on Stderr");
  expect(checked.code).toBe(0);
  expect(checked.stdout).toBe("");

  const [warning] = checked.stderr.split("\n");
  expect(warnings).toContain('```text title="Width Warning"\n');
  expect(fence(warnings, "Width Warning")).toBe(warning!);
  expect(checked.stderr).toBe(
    "stanza: cannot read the line width from .oxfmtrc.json, so the next source sets it\nchecked 1 file, no findings\n",
  );

  const guards = [
    "function f(ready: boolean, open: boolean) {",
    "  if (!ready) {",
    '    return failure("not ready");',
    "  }",
    "  if (!open) {",
    '    return failure("not open");',
    "  }",
    "  return 2;",
    "}",
    "",
  ];

  writeFileSync(join(cwd, "b.ts"), guards.join("\n"));
  expect(run({ cwd }, "--fix", "b.ts").code).toBe(0);
  expect(readFileSync(join(cwd, "b.ts"), "utf8")).toBe(
    [
      guards[0],
      guards[1]!.replace(" {", ""),
      guards[2],
      "",
      '  if (!open) return failure("not open");',
      guards[7],
      guards[8],
      "",
    ].join("\n"),
  );

  expect(warnings).toContain("takes the line width from the next source it checks");
});

test("the output reference excludes JSON stdin fix mode from width warnings", () => {
  const cwd = scratch("reference-width-json-stdin");
  writeFileSync(join(cwd, ".oxfmtrc.json"), "{");
  writeFileSync(join(cwd, ".prettierrc.json"), '{ "printWidth": 40 }');
  const source = "const value = 1;\n";

  const fixed = run(
    { cwd, stdin: new TextEncoder().encode(source) },
    "--fix",
    "--json",
    "--stdin",
    "a.ts",
  );

  expect(fixed).toEqual({ code: 0, stdout: source, stderr: "[]\n" });
  expect(section("/reference/output", "Warnings on Stderr")).toContain(
    "unless the run uses `--fix --json --stdin`",
  );
});

test("the explain reference quotes the beyond-EOF error under exit 2", () => {
  const cwd = scratch("reference-explain-eof");
  writeFileSync(join(cwd, "a.ts"), "const value = 1;\nvalue;\n");

  const explained = run({ cwd }, "explain", "a.ts:99");
  const status = section("/reference/explain", "Exit Status");
  expect(explained.code).toBe(2);
  expect(explained.stdout).toBe("");
  expect(explained.stderr).toBe("stanza: a.ts has 2 lines, not 99\n");

  expect(status).toContain('```text title="Beyond End of File"\n');
  expect(fence(status, "Beyond End of File")).toBe(explained.stderr.trimEnd());
  expect(status).toContain("`stanza explain a.ts:99`");
  expect(tableAfter(status, ["exit", "meaning"]).find(([exit]) => exit === "2")?.[1]).toContain(
    "a line beyond the end of the file",
  );
});

test("the CLI reference worker range and threshold match worker selection", () => {
  const environment = section("/reference/cli", "Environment");
  const range = /unsigned integer from (\d+) to (\d+) inclusive/.exec(environment);
  const threshold = /below (\d+) files/.exec(environment);
  expect(range).not.toBeNull();
  expect(threshold).not.toBeNull();

  const minimum = Number(range![1]);
  const maximum = Number(range![2]);
  const below = Number(threshold![1]);
  expect(minimum).toBe(0);
  expect(environment).toContain("accepts only digits");

  expect(environment).toContain("`0` runs serially on the main thread, without worker threads");
  expect(environment).toContain("Any accepted value applies at any file count");
  expect(environment).toContain("Invalid or out-of-range values are ignored");

  expect(environment).toContain(
    "uses one worker thread fewer than the available parallelism, up to eight",
  );

  expect(environment).toContain(
    "Only runs over path arguments or `--changed` can use worker threads. `--staged`, `--stdin` and hook runs never do.",
  );

  for (const term of ["selection", "large-run threshold"]) expect(environment).not.toContain(term);
  expect(catalog.workers).toEqual({ default: 8, most: maximum, threshold: below });
});

test("the CLI reference documents the hook off switch", () => {
  const environment = section("/reference/cli", "Environment");
  expect(environment).toContain("`AGENT_HOOKS=0` turns the hook off");
  expect(run({ env: { AGENT_HOOKS: "0" } }, "hook", "--unexpected")).toEqual({
    code: 0,
    stdout: "",
    stderr: "",
  });
});

const findings = /^wall\.ts:\d+:\d+: \S+: /;

test("the output page states the stream each stdin mode writes to", () => {
  const cwd = scratch("reference");

  const fixed = run({ cwd, stdin: unfixed }, "--fix", "--stdin", "wall.ts");
  expect(fixed.stdout).toBe(wall);
  expect(fixed.stderr).toMatch(findings);

  const checked = run({ cwd, stdin: unfixed }, "--check", "--stdin", "wall.ts");
  expect(checked.stdout).toMatch(findings);
  expect(checked.stderr).toBe("");

  expect(rawTableAfter(page("/reference/output").processed, ["mode", "stdout", "stderr"])).toEqual([
    ["`--fix --stdin`", "the fixed text", "findings"],
    ["`--check --stdin`", "findings", "warnings only"],
  ]);
});
