import { expect, test } from "bun:test";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { texts } from "./docs-pages.ts";
import { tableAfter } from "./rule-tables.ts";
import { dev, run, scratch } from "./support.ts";

function workers(files: number[], value?: string, cores = 64): number[] {
  const env = { ...process.env, STANZA_WORKERS: value };
  return dev(process.cwd(), ["workers", "--cores", String(cores), ...files.map(String)], {
    env,
  }).map((line) => (JSON.parse(line) as { workers: number }).workers);
}

function section(url: string, heading: string): string {
  const content = texts[url]?.raw.split(`## ${heading}\n`)[1]?.split("\n## ")[0];
  expect(content, `${url} has no ${heading} section`).toBeDefined();
  return content!;
}

function output(text: string, title: string): string {
  const content = text.split(`\`\`\`text title="${title}"\n`)[1]?.split("\n```")[0];
  expect(content, `no text block titled ${title}`).toBeDefined();
  return content!;
}

test("the output reference quotes the width warning and its fallback", () => {
  const cwd = scratch("reference-width");
  writeFileSync(join(cwd, ".oxfmtrc.json"), "{");
  writeFileSync(join(cwd, ".prettierrc.json"), '{ "printWidth": 40 }');
  writeFileSync(join(cwd, "a.ts"), "const value = 1;\n");

  const checked = run({ cwd }, "--check", "a.ts");
  const warnings = section("/reference/output", "Warnings on Stderr");
  expect(checked.code).toBe(0);
  expect(checked.stdout).toBe("");
  expect(output(warnings, "Width Warning")).toBe(checked.stderr.trimEnd());
  expect(checked.stderr).toBe(
    "stanza: could not read the line width from .oxfmtrc.json, so stanza used the next source for it\n",
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

  expect(output(status, "Beyond End of File")).toBe(explained.stderr.trimEnd());
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
  expect(workers([below - 1, below, below * 2])).toEqual([0, 8, 8]);

  for (const [cores, count] of [
    [1, 0],
    [2, 1],
    [6, 5],
    [9, 8],
  ] as const)
    expect(workers([below - 1, below], undefined, cores)).toEqual([0, count]);

  const counts = [1, below * 2];
  for (let count = minimum; count <= maximum; count++)
    expect(workers(counts, String(count))).toEqual([count, count]);

  for (const value of [String(maximum + 1), "-1", "+1", "1.5", " 2", "2 ", "2x", ""])
    expect(workers(counts, value), `STANZA_WORKERS=${JSON.stringify(value)}`).toEqual(
      workers(counts),
    );

  expect(workers([1], "00")).toEqual([0]);
  expect(workers([1], "01")).toEqual([1]);
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

test("the CLI reference leaves out STANZA_RAW_TRANSFER, which changes no output", () => {
  expect(texts["/reference/cli"]!.raw).not.toContain("STANZA_RAW_TRANSFER");

  const cwd = scratch("reference-raw-transfer");
  writeFileSync(join(cwd, "a.ts"), `if (a) {\n  b();\n}\n${" ".repeat(500_000)}\n`);
  const results = [undefined, "0"].map((mode) =>
    run({ cwd, env: { ...process.env, STANZA_RAW_TRANSFER: mode } }, "--check", "a.ts"),
  );

  expect(results[0]!.code).toBe(1);
  expect(results[1]).toEqual(results[0]!);
});
