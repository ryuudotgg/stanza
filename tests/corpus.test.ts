import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { chmodSync, cpSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { batchClient, type BatchRequest } from "../scripts/batch.ts";
import {
  agrees,
  coversEveryLine,
  type Fix,
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
import { batch, batchCommand, devBinary, run, scratch } from "./support.ts";

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

async function judged(path: string, text: string, braces: BatchRequest["braces"] = null) {
  const client = batchClient(batchCommand());
  const errors: string[] = [];
  const verdict = await judgeText(path, text, braces, client, errors);
  expect([...errors, ...(await client.close())]).toEqual([]);
  return verdict;
}

const quiet = { explain: async () => [] };

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

    expect(verdict).toEqual({
      kind: "judged",
      output: undefined,
      findings: undefined,
      explain: undefined,
      broken: ["crash"],
    });
  });

  test("finding messages change the judged findings", async () => {
    const input = "run();\n";
    const reporting =
      (message: string): Fix =>
      async (path, text, mode) => ({
        text,
        findings:
          mode === "check"
            ? [{ path, line: 1, col: 1, rule: "error", message, fixable: false }]
            : [],
        parseError: false,
      });

    const first = await judge("a.ts", input, { ...quiet, fix: reporting("first") });
    const second = await judge("a.ts", input, { ...quiet, fix: reporting("second") });
    expect(first).toMatchObject({ kind: "judged", output: input });
    expect(second).toMatchObject({ kind: "judged", output: input });

    if (first.kind !== "judged" || second.kind !== "judged")
      throw new Error("expected judged verdicts");

    expect(first.findings).not.toBe(second.findings);
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
    expect(await judged(beforePath, before)).toEqual({
      kind: "judged",
      output: after,
      findings: expect.any(String),
      explain: expect.any(String),
      broken: [],
    });
  });
});

const corpusScript = join(import.meta.dir, "..", "scripts", "corpus.ts");

describe("corpus snapshots", () => {
  let spent = 0;
  beforeEach(() => {
    spent = 0;
  });

  afterEach(() => {
    expect(spent).toBeLessThan(5_000);
  });

  function corpus(cwd: string, ...args: string[]) {
    const result = Bun.spawnSync(["bun", corpusScript, "--binary", devBinary(), ...args], {
      cwd,
      env: process.env,
    });

    spent += cpuOf(result);
    return {
      exitCode: result.exitCode,
      stdout: result.stdout.toString(),
      stderr: result.stderr.toString(),
    };
  }

  test("generated fixtures require an explicit selection", () => {
    const root = join(import.meta.dir, "..");
    const skipped = corpus(root, "tests/fixtures");
    expect(skipped.exitCode).toBe(2);
    expect(skipped.stderr).toContain("selected no files");

    const included = corpus(root, "--include-generated", "tests/fixtures");
    expect(included.exitCode).toBe(0);
    expect(Number(/files: (\d+)/.exec(included.stdout)?.[1])).toBeGreaterThan(0);
  }, 60_000);

  test("a generated header is skipped unless generated files are included", () => {
    const directory = scratch("corpus");
    const tree = join(directory, "tree");

    mkdirSync(tree);
    writeFileSync(join(tree, "plain.ts"), "if (a) {\n  b();\n}\n");
    writeFileSync(join(tree, "gen.ts"), "﻿// @generated\nif (a) {\n  b();\n}\n");

    const skipped = corpus(directory, "tree");
    expect(skipped.exitCode).toBe(0);
    expect(skipped.stdout).toContain("files: 1\n");

    const included = corpus(directory, "--include-generated", "tree");
    expect(included.exitCode).toBe(0);
    expect(included.stdout).toContain("files: 2\n");
  }, 60_000);

  test("records compare across checkouts and changes fail the run", () => {
    const directory = scratch("corpus");
    const first = join(directory, "first");
    const second = join(directory, "second");
    const record = join(directory, "record.json");

    for (const checkout of [first, second])
      cpSync(dir, join(checkout, "tree"), { recursive: true });

    expect(corpus(first, "--snapshot", record, "tree").exitCode).toBe(0);

    const same = corpus(second, "--against", record, "tree");
    expect(same.stdout).toContain("changed: 0");
    expect(same.exitCode).toBe(0);

    writeFileSync(join(second, "tree", "nested.after.ts"), "run();\n");

    const changed = corpus(second, "--against", record, "tree");
    expect(changed.stdout).toContain("differs (output): tree/nested.after.ts\nchanged: 1");
    expect(changed.exitCode).toBe(1);
  }, 60_000);

  test("against names findings and explanations without output changes", () => {
    const directory = scratch("corpus");
    const tree = join(directory, "tree");
    const record = join(directory, "record.json");

    cpSync(dir, tree, { recursive: true });
    expect(corpus(directory, "--snapshot", record, "tree").exitCode).toBe(0);

    const entries: Record<string, { output: string; findings: string; explain: string }> =
      JSON.parse(readFileSync(record, "utf8"));

    const path = Object.keys(entries)[0]!;

    entries[path]!.findings = "changed";
    entries[path]!.explain = "changed";
    writeFileSync(record, JSON.stringify(entries));

    const result = corpus(directory, "--against", record, "tree");
    expect(result.exitCode).toBe(1);
    expect(result.stdout).toContain(`differs (findings, explain): ${path}\nchanged: 1`);
  }, 60_000);

  test("a byte order mark is judged in place, as stanza --fix writes it", () => {
    const directory = scratch("corpus");
    const path = join(directory, "tree", "a.ts");
    const record = join(directory, "record.json");
    const input = "\uFEFF#!/usr/bin/env bun\nconst a = 1;\nif (a) {\n  b();\n}\nconst c = 2;\n";

    mkdirSync(dirname(path));
    writeFileSync(path, input);
    expect(corpus(directory, "--snapshot", record, "tree").exitCode).toBe(0);

    expect(run({ cwd: directory }, "--fix", "tree/a.ts").code).toBe(0);
    const written = readFileSync(path);
    expect(written.toString()).not.toBe(input);

    const hashes: Record<string, { output: string; findings: string; explain: string }> =
      JSON.parse(readFileSync(record, "utf8"));

    expect(hashes[join("tree", "a.ts")]!.output).toBe(
      createHash("sha256").update(written).digest("hex"),
    );
  }, 60_000);

  test("snapshot writes hashes for output, findings and explanations", () => {
    const directory = scratch("corpus");
    const tree = join(directory, "tree");
    const record = join(directory, "record.json");

    cpSync(dir, tree, { recursive: true });
    expect(corpus(directory, "--snapshot", record, "tree").exitCode).toBe(0);

    const entries: Record<string, Record<string, unknown>> = JSON.parse(
      readFileSync(record, "utf8"),
    );

    for (const entry of Object.values(entries)) {
      expect(Object.keys(entry)).toEqual(["output", "findings", "explain"]);
      expect(typeof entry.output).toBe("string");
      expect(typeof entry.findings).toBe("string");
      expect(typeof entry.explain).toBe("string");
    }
  }, 60_000);

  test("earlier output only records require a new snapshot", () => {
    const directory = scratch("corpus");
    const tree = join(directory, "tree");
    const record = join(directory, "record.json");

    cpSync(dir, tree, { recursive: true });
    writeFileSync(record, JSON.stringify({ "tree/a.ts": "abc" }));

    const result = corpus(directory, "--against", record, "tree");
    expect(result.exitCode).toBe(2);
    expect(result.stderr).toContain(record);
    expect(result.stderr).toContain("take a new snapshot");
  }, 60_000);

  test("an unreadable file leaves the previous snapshot in place", () => {
    const directory = scratch("corpus");
    const tree = join(directory, "tree");
    const record = join(directory, "record.json");
    const locked = join(tree, "nested.after.ts");

    cpSync(dir, tree, { recursive: true });
    expect(corpus(directory, "--snapshot", record, "tree").exitCode).toBe(0);

    const baseline = readFileSync(record, "utf8");
    chmodSync(locked, 0o000);

    expect(corpus(directory, "--snapshot", record, "tree").exitCode).toBe(2);
    expect(readFileSync(record, "utf8")).toBe(baseline);
  }, 60_000);
});

function corpusCommand(directory: string, args: string[]) {
  return Bun.spawnSync([process.execPath, corpusScript, ...args], {
    cwd: directory,
    env: { ...process.env, FORCE_COLOR: undefined },
    timeout: 60_000,
  });
}

test("a binary that stops answering fails the corpus run with its exit code", () => {
  const directory = scratch("corpus-dead-binary");
  const binary = join(directory, "dead");
  writeFileSync(
    binary,
    `#!/bin/sh\nif [ "$1" = select ]; then exec ${JSON.stringify(devBinary())} "$@"; fi\nexit 3\n`,
  );

  chmodSync(binary, 0o755);

  const judged = corpusCommand(directory, ["--binary", binary, dir]);
  expect(judged.stderr.toString()).toContain(`${binary} batch exited with code 3`);
  expect(judged.exitCode).toBe(2);
});
