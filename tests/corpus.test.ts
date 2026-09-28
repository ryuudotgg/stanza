import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { chmodSync, cpSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import {
  agrees,
  coversEveryLine,
  type Fix,
  isIdempotent,
  judge,
  keepsDirectives,
  leavesNothingFixable,
  preservesShape,
  preservesText,
  side,
  touchesNoLine,
} from "../scripts/corpus.ts";
import { keepBraces as bracesFor } from "../src/step.ts";
import { run, scratch } from "./support.ts";

const dir = join(import.meta.dir, "fixtures", "braces");
const beforePath = join(dir, "bodies.before.ts");
const before = readFileSync(beforePath, "utf8");
const after = readFileSync(join(dir, "bodies.after.ts"), "utf8");
const keepBraces = bracesFor(beforePath);
const options = { keepBraces };

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

  test("idempotence: the fixed side still moves", () => {
    expect(isIdempotent(beforePath, before, options)).toBe(false);
  });

  test("program shape: the fixed side dropped a statement", () => {
    expect(keepsShape("first();\nsecond();\n", "first();\n")).toBe(false);
    expect(keepsShape("if (a) {\n  b();\n  ;\n}\n", "if (a) {\n  b();\n}\n")).toBe(false);
  });

  test("program shape: the fixed side no longer parses", () => {
    expect(keepsShape("run();\n", "run(;\n")).toBe(false);
  });

  test("fixable left: the fixed side still carries a fixable finding", () => {
    expect(leavesNothingFixable(beforePath, before, options)).toBe(false);
  });

  test("agreement: check omits a fixable edit", () => {
    const input = "if (a) { b(); }\n";
    const output = "if (a) b();\n";
    const fix: Fix = (_path, source) => ({ text: source, findings: [], parseError: false });

    expect(
      agrees("a.ts", input, output, side("a.ts", input), side("a.ts", output), options, fix),
    ).toBe(false);
  });

  test("agreement: check misplaces a blank line edit or turns it around", () => {
    const input = "function f() {\n  a();\n\n  b();\n}\n";
    const output = "function f() {\n  a();\n  b();\n}\n";
    const reporting =
      (line: number, rule: "short-body" | "after-multiline"): Fix =>
      () => ({
        text: input,
        findings: [{ path: "a.ts", line, col: 3, rule, message: "", fixable: true }],
        parseError: false,
      });

    const judged = (fix: Fix) =>
      agrees("a.ts", input, output, side("a.ts", input), side("a.ts", output), options, fix);

    expect(judged(reporting(4, "short-body"))).toBe(true);
    expect(judged(reporting(2, "short-body"))).toBe(false);
    expect(judged(reporting(4, "after-multiline"))).toBe(false);
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

  test("empty hunk: a no-change run altered a line", () => {
    const fix: Fix = (_path, input, _mode, settings) => ({
      text: settings.changedLines?.lines.size === 0 ? "changed();\n" : input,
      findings: [],
      parseError: false,
    });

    expect(touchesNoLine("a.ts", "run();\n", options, fix)).toBe(false);
  });

  test("full hunk: a scoped run differs from the full fix", () => {
    const fix: Fix = (_path, input, _mode, settings) => ({
      text: settings.changedLines ? input : "fixed();\n",
      findings: [],
      parseError: false,
    });

    expect(coversEveryLine("a.ts", "run();\n", "fixed();\n", options, fix)).toBe(false);
  });

  test("crash: a throwing fix is reported", () => {
    const verdict = judge("a.ts", "run();\n", false, () => {
      throw new RangeError("Maximum call stack size exceeded");
    });

    expect(verdict).toEqual({ kind: "judged", output: undefined, broken: ["crash"] });
  });
});

describe("corpus invariants accept bodies.before against bodies.after", () => {
  test("braces are not enforced for the fixture", () => {
    expect(keepBraces).toBe(false);
  });

  test("each check holds", () => {
    expect(isIdempotent(beforePath, after, options)).toBe(true);
    expect(preservesText(side(beforePath, before), side(beforePath, after))).toBe(true);
    expect(preservesShape(side(beforePath, before), side(beforePath, after))).toBe(true);
    expect(leavesNothingFixable(beforePath, after, options)).toBe(true);
    expect(
      agrees(beforePath, before, after, side(beforePath, before), side(beforePath, after), options),
    ).toBe(true);

    expect(coversEveryLine(beforePath, before, after, options)).toBe(true);
    expect(touchesNoLine(beforePath, before, options)).toBe(true);
  });

  test("ignore fixtures keep directives and judge cleanly", () => {
    for (const name of ["ignore", "labels"]) {
      const path = join(import.meta.dir, "fixtures", "ignore", `${name}.before.ts`);
      const input = readFileSync(path, "utf8");
      const output = readFileSync(
        join(import.meta.dir, "fixtures", "ignore", `${name}.after.ts`),
        "utf8",
      );

      expect(judge(path, input, false)).toMatchObject({ kind: "judged", broken: [] });
      expect(keepsDirectives(side(path, input), side(path, output))).toBe(true);
    }
  });

  test("compact braces, BigInt literals and a CRLF blank line pass", () => {
    for (const input of [
      "if (a) {b();}\n",
      "if(a){b();}\n",
      "if (a)  { b(); }\n",
      "if (a)\t{ b(); }\n",
    ])
      expect(judge("a.ts", input, false)).toMatchObject({ kind: "judged", broken: [] });

    expect(keepsText("if (a) {\n  b(x);\n}\n", "if (a)\n  b(x) ;\n")).toBe(false);

    expect(keepsText("a();\r\nb();\r\n", "a();\r\n\r\nb();\r\n")).toBe(true);
    expect(keepsShape("const big = 10n;\n", "const big = 10n;\n")).toBe(true);
    expect(keepsShape("const big = 10n;\n", "const big = 11n;\n")).toBe(false);
  });

  test("judge breaks nothing and produces the after text", () => {
    expect(judge(beforePath, before, keepBraces)).toEqual({
      kind: "judged",
      output: after,
      broken: [],
    });
  });
});

describe("corpus snapshots", () => {
  const script = join(import.meta.dir, "..", "scripts", "corpus.ts");
  function corpus(cwd: string, ...args: string[]) {
    const result = Bun.spawnSync(["bun", script, ...args], { cwd });
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
  });

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
    expect(changed.stdout).toContain("differs: tree/nested.after.ts\nchanged: 1");
    expect(changed.exitCode).toBe(1);
  });

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

    const hashes: Record<string, string> = JSON.parse(readFileSync(record, "utf8"));
    expect(hashes[join("tree", "a.ts")]).toBe(createHash("sha256").update(written).digest("hex"));
  });

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
  });
});
