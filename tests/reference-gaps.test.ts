import { expect, test } from "bun:test";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { formatterWidth } from "../src/languages/javascript/config/width.ts";
import { DEFAULT_WORKERS, THRESHOLD, workerCount } from "../src/pool.ts";
import { texts } from "./docs-pages.ts";
import { tableAfter } from "./rule-tables.ts";
import { run, scratch } from "./support.ts";

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

  expect(formatterWidth(join(cwd, "a.ts"))).toMatchObject({
    columns: 40,
    source: { kind: "config", file: join(cwd, ".prettierrc.json") },
    unread: [join(cwd, ".oxfmtrc.json")],
  });

  expect(warnings).toContain("takes the line width from the next source it checks");
});

test("the explain reference quotes the beyond-EOF error under exit 2", () => {
  const cwd = scratch("reference-explain-eof");
  writeFileSync(join(cwd, "a.ts"), "const value = 1;\nvalue;");

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
  expect(minimum).toBe(0);
  expect(Number(threshold![1])).toBe(THRESHOLD);
  expect(environment).toContain("accepts only digits");

  expect(environment).toContain("`0` runs serially on the main thread, without worker threads");
  expect(environment).toContain("Any accepted value overrides the large-run threshold");
  expect(environment).toContain("Invalid or out-of-range values are ignored");

  expect(environment).toContain("up to three worker threads");
  expect(environment).toContain("depending on available parallelism");
  expect(DEFAULT_WORKERS).toBe(3);

  for (const fileCount of [1, THRESHOLD * 2]) {
    for (let count = minimum; count <= maximum; count++)
      expect(workerCount(fileCount, { STANZA_WORKERS: String(count) })).toBe(count);

    for (const value of [String(maximum + 1), "-1", "+1", "1.5", " 2", "2 ", "2x", ""])
      expect(workerCount(fileCount, { STANZA_WORKERS: value })).toBe(workerCount(fileCount, {}));
  }

  expect(workerCount(1, { STANZA_WORKERS: "00" })).toBe(0);
  expect(workerCount(1, { STANZA_WORKERS: "01" })).toBe(1);
  expect(workerCount(THRESHOLD - 1, {})).toBe(0);
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

test("the CLI reference exposes only the public raw transfer off switch", () => {
  const environment = section("/reference/cli", "Environment");
  expect(environment).toContain("`STANZA_RAW_TRANSFER=0` disables Oxc raw AST transfer");
  expect(environment).toContain("uses JSON transfer instead");
  expect(texts["/reference/cli"]!.raw).not.toMatch(/STANZA_RAW_TRANSFER(?:=|`\s*=\s*`)1/);

  const parser = join(import.meta.dir, "..", "src", "languages", "javascript", "parse.ts");
  const raw = join(import.meta.dir, "..", "src", "languages", "javascript", "raw.ts");
  const probe = `
    import { mock } from "bun:test";
    let rawCalls = 0;
    mock.module(${JSON.stringify(raw)}, () => ({
      parseRaw() {
        rawCalls++;
        throw new Error("raw transfer probe");
      },
    }));
    const { parse } = await import(${JSON.stringify(parser)});
    const parsed = parse("a.ts", "const value = 1;" + " ".repeat(500_000));
    console.log(JSON.stringify({ rawCalls, errors: parsed.errors, statements: parsed.program.body.length }));
  `;

  for (const mode of [undefined, "0"]) {
    const result = Bun.spawnSync([process.execPath, "--eval", probe], {
      env: { ...process.env, STANZA_RAW_TRANSFER: mode },
    });

    expect(new TextDecoder().decode(result.stderr)).toBe("");
    expect(result.exitCode).toBe(0);
    expect(JSON.parse(new TextDecoder().decode(result.stdout))).toEqual({
      rawCalls: mode === "0" ? 0 : 1,
      errors: [],
      statements: 1,
    });
  }
});
