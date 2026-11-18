import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  chmodSync,
  cpSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { extname, join } from "node:path";
import catalog from "../docs/lib/catalog.json";
import {
  batchClient,
  type BatchClient,
  type BatchRequest,
  type BatchResponse,
} from "../scripts/batch.ts";
import {
  agrees,
  coversEveryLine,
  type Fix,
  INVARIANTS,
  isIdempotent,
  judge,
  judgeText,
  keepsDirectives,
  leavesNothingFixable,
  preservesShape,
  preservesText,
  side,
  touchesNoLine,
} from "../scripts/corpus.ts";
import { cpuOf } from "./budget.ts";
import { batch, batchCommand, colorless, devBinary, run, scratch } from "./support.ts";

const dir = join(import.meta.dir, "fixtures", "braces");
const beforePath = join(dir, "bodies.before.ts");
const before = readFileSync(beforePath, "utf8");
const after = readFileSync(join(dir, "bodies.after.ts"), "utf8");

const fix: Fix = async (path, text, mode, changedLines) => {
  const [answer] = batch(process.cwd(), [
    {
      path,
      text,
      mode,
      braces: null,
      changedLines: changedLines
        ? { lines: [...changedLines.lines], deletedAfter: [...changedLines.deletedAfter] }
        : null,
      explain: [],
    },
  ]);

  if (answer?.text == null) throw new Error(answer?.error ?? "no answer");
  return {
    text: answer.text,
    findings: answer.findings,
    parseError: answer.findings.some((finding) => finding.rule === "parse"),
  };
};

function answering(alter: (response: BatchResponse) => BatchResponse): BatchClient {
  const client = batchClient(batchCommand());
  return {
    async ask(request) {
      const response = await client.ask(request);
      return request.explain.length > 0 ? alter(response) : response;
    },
    close: () => client.close(),
  };
}

async function explainedBy(alter: (response: BatchResponse) => BatchResponse) {
  const client = answering(alter);
  const log: string[] = [];
  const verdict = await judgeText("a.ts", "if (a) { b(); }\n", null, client, log);
  expect(await client.close()).toEqual([]);
  return { verdict, log };
}

async function judged(path: string, text: string, braces: BatchRequest["braces"] = null) {
  const client = batchClient(batchCommand());
  const errors: string[] = [];
  const verdict = await judgeText(path, text, braces, client, errors);
  expect([...errors, ...(await client.close())]).toEqual([]);
  return verdict;
}

const quiet = { explain: async () => true };

function keepsText(original: string, fixed: string): boolean {
  return preservesText(side("a.ts", original), side("a.ts", fixed));
}

function keepsShape(original: string, fixed: string): boolean {
  return preservesShape(side("a.ts", original), side("a.ts", fixed));
}

function keepsDirectivesOf(original: string, fixed: string): boolean {
  return keepsDirectives(side("a.ts", original), side("a.ts", fixed));
}

describe("corpus invariants reject a broken pair", () => {
  test("preservation: a changed character inside a line", () => {
    expect(keepsText("const total = 1;\n", "const totál = 1;\n")).toBe(false);
    expect(keepsText("if (a) {\n  b(x);\n}\n", "if (a)\n  b( x);\n")).toBe(false);
  });

  test("preservation: a changed comment byte", () => {
    expect(keepsText("// keep\nrun();\n", "// kept\nrun();\n")).toBe(false);
    expect(keepsText("/* one\n\n  two */\nrun();\n", "/* one\n  two */\nrun();\n")).toBe(false);
  });

  test("preservation: a blank line with the wrong line ending", () => {
    expect(keepsText("a();\r\nb();\r\n", "a();\r\n\nb();\r\n")).toBe(false);
  });

  test("preservation: a dropped final newline", () => {
    expect(keepsText("a();\n", "a();")).toBe(false);
  });

  test("idempotence: the fixed side still moves", async () => {
    expect(await isIdempotent(beforePath, before, fix)).toBe(false);
  });

  test("program shape: the fixed side dropped a statement", () => {
    expect(keepsShape("first();\nsecond();\n", "first();\n")).toBe(false);
    expect(keepsShape("if (a) {\n  b();\n  ;\n}\n", "if (a) {\n  b();\n}\n")).toBe(false);
  });

  test("program shape: the fixed side no longer parses", () => {
    expect(keepsShape("run();\n", "run(;\n")).toBe(false);
  });

  test("fixable left: the fixed side still carries a fixable finding", async () => {
    expect(await leavesNothingFixable(beforePath, before, fix)).toBe(false);
  });

  test("agreement: check omits a fixable edit", () => {
    const input = "if (a) { b(); }\n";
    const output = "if (a) b();\n";
    expect(agrees(input, output, side("a.ts", input), side("a.ts", output), [])).toBe(false);
  });

  test("agreement: check misplaces a blank line edit or turns it around", () => {
    const input = "function f() {\n  a();\n\n  b();\n}\n";
    const output = "function f() {\n  a();\n  b();\n}\n";
    const judged = (line: number, rule: "short-body" | "after-multiline") =>
      agrees(input, output, side("a.ts", input), side("a.ts", output), [
        { path: "a.ts", line, col: 3, rule, message: "", fixable: true },
      ]);

    expect(judged(4, "short-body")).toBe(true);
    expect(judged(2, "short-body")).toBe(false);
    expect(judged(4, "after-multiline")).toBe(false);
  });

  test("directives: a frozen region changed", () => {
    expect(
      keepsDirectivesOf(
        "/* stanza-off */\nif (a) {\n  b();\n}\n/* stanza-on */\n",
        "/* stanza-off */\nif (a) {\n\n  b();\n}\n/* stanza-on */\n",
      ),
    ).toBe(false);

    expect(
      keepsDirectivesOf(
        "// stanza-ignore\nif (a) {\n  b();\n}\n",
        "// stanza-ignore\nif (a)\n  b();\n",
      ),
    ).toBe(false);

    expect(
      keepsDirectivesOf(
        "// stanza-ignore\nrun();\n\nnext();\n",
        "// stanza-ignore\nrun();\nnext();\n",
      ),
    ).toBe(false);

    expect(
      keepsDirectivesOf(
        "/* stanza-off */\n/* stanza-off */\n/* stanza-on */\nrun();\n\nnext();\n/* stanza-on */\n",
        "/* stanza-off */\n/* stanza-off */\n/* stanza-on */\nrun();\nnext();\n/* stanza-on */\n",
      ),
    ).toBe(false);

    expect(
      keepsDirectivesOf(
        "// stanza-ignore\n// explanation\nif (a) {\n  b();\n}\n",
        "// stanza-ignore\n// explanation\nif (a)\n  b();\n",
      ),
    ).toBe(false);

    expect(
      keepsDirectivesOf(
        "switch (a) {\n  // stanza-ignore\n  case 1:\n    run();\n\n  case 2:\n    break;\n}\n",
        "switch (a) {\n  // stanza-ignore\n  case 1:\n    run();\n  case 2:\n    break;\n}\n",
      ),
    ).toBe(false);

    expect(
      keepsDirectivesOf(
        "function run() {\n\n  // stanza-ignore\n  go();\n\n}\n",
        "function run() {\n  // stanza-ignore\n  go();\n}\n",
      ),
    ).toBe(false);
  });

  test("empty hunk: a no-change run altered a line", async () => {
    const touching: Fix = async (_path, input, _mode, changedLines) => ({
      text: changedLines?.lines.size === 0 ? "changed();\n" : input,
      findings: [],
      parseError: false,
    });

    expect(await touchesNoLine("a.ts", "run();\n", touching)).toBe(false);
  });

  test("full hunk: a scoped run differs from the full fix", async () => {
    const scoped: Fix = async (_path, input, _mode, changedLines) => ({
      text: changedLines ? input : "fixed();\n",
      findings: [],
      parseError: false,
    });

    expect(await coversEveryLine("a.ts", "run();\n", "fixed();\n", scoped)).toBe(false);
  });

  test("crash: a throwing fix is reported", async () => {
    const verdict = await judge("a.ts", "run();\n", {
      ...quiet,
      fix: async () => {
        throw new RangeError("Maximum call stack size exceeded");
      },
    });

    expect(verdict).toEqual({ kind: "judged", output: undefined, broken: ["crash"] });
  });

  test("explain: the binary drops the explanation of a finding line", async () => {
    const { verdict, log } = await explainedBy((response) => ({ ...response, explanations: {} }));
    expect(verdict).toEqual({ kind: "judged", output: expect.any(String), broken: ["explain"] });
    expect(log).toEqual(["a.ts: the binary did not explain line 1"]);
  });

  test("explain: the binary answers the explanation with an error", async () => {
    const { verdict, log } = await explainedBy((response) => ({
      ...response,
      error: "explain failed",
    }));

    expect(verdict).toEqual({ kind: "judged", output: expect.any(String), broken: ["explain"] });
    expect(log).toEqual(["a.ts: explain failed"]);
  });
});

describe("corpus invariants accept bodies.before against bodies.after", () => {
  test("each check holds", async () => {
    expect(await isIdempotent(beforePath, after, fix)).toBe(true);
    expect(preservesText(side(beforePath, before), side(beforePath, after))).toBe(true);
    expect(preservesShape(side(beforePath, before), side(beforePath, after))).toBe(true);
    expect(await leavesNothingFixable(beforePath, after, fix)).toBe(true);
    expect(
      agrees(
        before,
        after,
        side(beforePath, before),
        side(beforePath, after),
        (await fix(beforePath, before, "check")).findings,
      ),
    ).toBe(true);

    expect(await coversEveryLine(beforePath, before, after, fix)).toBe(true);
    expect(await touchesNoLine(beforePath, before, fix)).toBe(true);
  });

  test("ignore fixtures keep directives and judge cleanly", async () => {
    for (const name of ["ignore", "labels"]) {
      const path = join(import.meta.dir, "fixtures", "ignore", `${name}.before.ts`);
      const input = readFileSync(path, "utf8");
      const output = readFileSync(
        join(import.meta.dir, "fixtures", "ignore", `${name}.after.ts`),
        "utf8",
      );

      expect(await judged(path, input)).toMatchObject({ kind: "judged", broken: [] });
      expect(keepsDirectives(side(path, input), side(path, output))).toBe(true);
    }
  });

  test("compact braces, BigInt literals and a CRLF blank line pass", async () => {
    for (const input of [
      "if (a) {b();}\n",
      "if(a){b();}\n",
      "if (a)  { b(); }\n",
      "if (a)\t{ b(); }\n",
    ])
      expect(await judged("a.ts", input)).toMatchObject({ kind: "judged", broken: [] });

    expect(keepsText("if (a) {\n  b(x);\n}\n", "if (a)\n  b(x) ;\n")).toBe(false);

    expect(keepsText("a();\r\nb();\r\n", "a();\r\n\r\nb();\r\n")).toBe(true);
    expect(keepsShape("const big = 10n;\n", "const big = 10n;\n")).toBe(true);
    expect(keepsShape("const big = 10n;\n", "const big = 11n;\n")).toBe(false);
  });

  test("a parse failure the batch reports is the judge's parse failure verdict", async () => {
    expect(await judged("a.ts", "run(;\n")).toEqual({ kind: "parse failure" });
  });

  test("judge breaks nothing and produces the after text", async () => {
    expect(await judged(beforePath, before)).toEqual({ kind: "judged", output: after, broken: [] });
  });

  test("a byte order mark is judged in place, as stanza --fix writes it", async () => {
    const directory = scratch("corpus");
    const path = join(directory, "a.ts");
    const input = "\uFEFF#!/usr/bin/env bun\nif (a) { b(); }\n";

    writeFileSync(path, input);
    const verdict = await judged(path, input);

    expect(run({ cwd: directory }, "--fix", "a.ts").code).toBe(0);
    const written = readFileSync(path).toString("utf8");
    expect(written).not.toBe(input);
    expect(written.startsWith("\uFEFF")).toBe(true);
    expect(verdict).toEqual({ kind: "judged", output: written, broken: [] });
  });
});

const corpusScript = join(import.meta.dir, "..", "scripts", "corpus.ts");
const repository = join(import.meta.dir, "..");
function sources(directory: string): number {
  const extensions = new Set(catalog.extensions);
  return readdirSync(join(repository, directory), { recursive: true, withFileTypes: true }).filter(
    (entry) => entry.isFile() && extensions.has(extname(entry.name)),
  ).length;
}

describe("corpus runs", () => {
  let spent = 0;
  beforeEach(() => {
    spent = 0;
  });

  afterEach(() => {
    expect(spent).toBeLessThan(5_000);
  });

  function corpus(cwd: string, args: string[], binary = devBinary()) {
    const result = Bun.spawnSync([process.execPath, corpusScript, "--binary", binary, ...args], {
      cwd,
      env: colorless(),
      timeout: 60_000,
    });

    spent += cpuOf(result);
    if (result.exitedDueToTimeout) throw new Error("corpus.ts ran past 60 seconds");

    return {
      exitCode: result.exitCode,
      stdout: result.stdout.toString(),
      stderr: result.stderr.toString(),
    };
  }

  test("every invariant holds over the fixtures and scripts with braces on", () => {
    const result = corpus(repository, [
      "--include-generated",
      "--braces",
      "tests/fixtures",
      "scripts",
    ]);

    const report = `stdout:\n${result.stdout}\nstderr:\n${result.stderr}`;
    expect(result.exitCode, report).toBe(0);
    expect(Number(/^files: (\d+)$/m.exec(result.stdout)?.[1]), report).toBe(
      sources("tests/fixtures") + sources("scripts"),
    );

    for (const invariant of INVARIANTS)
      expect(new RegExp(`^${invariant}: (\\d+)$`, "m").exec(result.stdout)?.[1], report).toBe("0");

    const unparsed = [...result.stdout.matchAll(/^ {2}\.\w+ (\d+)\/\d+$/gm)].map(
      ([, count]) => count,
    );

    expect(unparsed.length, report).toBeGreaterThan(0);
    expect(unparsed, report).toEqual(unparsed.map(() => "0"));
  }, 60_000);

  test("generated fixtures require an explicit selection", () => {
    const skipped = corpus(repository, ["tests/fixtures"]);
    expect(skipped.exitCode).toBe(2);
    expect(skipped.stderr).toContain("selected no files");
  }, 60_000);

  test("a generated header is skipped unless generated files are included", () => {
    const directory = scratch("corpus");
    const tree = join(directory, "tree");

    mkdirSync(tree);
    writeFileSync(join(tree, "plain.ts"), "if (a) {\n  b();\n}\n");
    writeFileSync(join(tree, "gen.ts"), "﻿// @generated\nif (a) {\n  b();\n}\n");

    const skipped = corpus(directory, ["tree"]);
    expect(skipped.exitCode).toBe(0);
    expect(skipped.stdout).toContain("files: 1\n");

    const included = corpus(directory, ["--include-generated", "tree"]);
    expect(included.exitCode).toBe(0);
    expect(included.stdout).toContain("files: 2\n");
  }, 60_000);

  test.skipIf(process.getuid?.() === 0)(
    "an unreadable file fails the run and is named",
    () => {
      const directory = scratch("corpus");
      const tree = join(directory, "tree");
      const locked = join(tree, "nested.after.ts");

      cpSync(dir, tree, { recursive: true });

      const mode = statSync(locked).mode;
      chmodSync(locked, 0o000);

      try {
        const result = corpus(directory, ["tree"]);
        expect(result.exitCode).toBe(2);
        expect(result.stdout).toMatch(/^files: \d+$/m);
        expect(result.stderr).toContain(`${join("tree", "nested.after.ts")}: `);
      } finally {
        chmodSync(locked, mode);
      }
    },
    60_000,
  );

  test.each([
    ["drops the explanation", "explanations: {}", "the binary did not explain line 1"],
    ["answers the explanation with an error", 'error: "explain failed"', "explain failed"],
  ])("a binary that %s breaks explain and exits 1", (_, change, message) => {
    const directory = scratch("corpus-unexplained");
    const tree = join(directory, "tree");
    const filter = join(directory, "filter.ts");
    const binary = join(directory, "unexplained");
    const dev = JSON.stringify(devBinary());

    mkdirSync(tree);
    writeFileSync(join(tree, "a.ts"), "if (a) { b(); }\n");
    writeFileSync(
      filter,
      `for await (const line of console) {\n  if (line === "") continue;\n  const response = JSON.parse(line);\n  const changed = Object.keys(response.explanations).length > 0 ? { ...response, ${change} } : response;\n  console.log(JSON.stringify(changed));\n}\n`,
    );

    writeFileSync(
      binary,
      `#!/bin/sh\nif [ "$1" = batch ]; then ${dev} batch | ${JSON.stringify(process.execPath)} ${JSON.stringify(filter)}; exit 0; fi\nexec ${dev} "$@"\n`,
    );

    chmodSync(binary, 0o755);

    const judged = corpus(directory, ["tree"], binary);
    expect(judged.stdout).toContain(`explain: 1\n  ${join("tree", "a.ts")}\n`);
    expect(judged.stderr).toContain(`${join("tree", "a.ts")}: ${message}`);
    expect(judged.exitCode).toBe(1);
  });

  test("a binary that stops answering fails the corpus run with its exit code", () => {
    const directory = scratch("corpus-dead-binary");
    const binary = join(directory, "dead");
    writeFileSync(
      binary,
      `#!/bin/sh\nif [ "$1" = select ]; then exec ${JSON.stringify(devBinary())} "$@"; fi\nexit 3\n`,
    );

    chmodSync(binary, 0o755);

    const judged = corpus(directory, [dir], binary);
    expect(judged.stderr).toContain(`${binary} batch exited with code 3`);
    expect(judged.exitCode).toBe(2);
  });
});
