import { describe, expect, test } from "bun:test";
import { chmodSync, cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  isIdempotent,
  judge,
  leavesNothingFixable,
  preservesShape,
  preservesText,
  side,
} from "../scripts/corpus.ts";
import { bracesEnforced } from "../src/config/index.ts";

const dir = join(import.meta.dir, "fixtures", "braces");
const beforePath = join(dir, "bodies.before.ts");
const before = readFileSync(beforePath, "utf8");
const after = readFileSync(join(dir, "bodies.after.ts"), "utf8");
const keepBraces = bracesEnforced(dir, ".ts");
const options = { keepBraces };

function text(original: string, fixed: string): boolean {
  return preservesText(side("a.ts", original), side("a.ts", fixed));
}

function shape(original: string, fixed: string): boolean {
  return preservesShape(side("a.ts", original), side("a.ts", fixed));
}

describe("corpus invariants reject a broken pair", () => {
  test("preservation: a changed character inside a line", () => {
    expect(text("const total = 1;\n", "const totál = 1;\n")).toBe(false);
    expect(text("if (a) {\n  b(x);\n}\n", "if (a)\n  b( x);\n")).toBe(false);
  });

  test("preservation: a changed comment byte", () => {
    expect(text("// keep\nrun();\n", "// kept\nrun();\n")).toBe(false);
    expect(text("/* one\n\n  two */\nrun();\n", "/* one\n  two */\nrun();\n")).toBe(false);
  });

  test("preservation: a blank line with the wrong line ending", () => {
    expect(text("a();\r\nb();\r\n", "a();\r\n\nb();\r\n")).toBe(false);
  });

  test("preservation: a dropped final newline", () => {
    expect(text("a();\n", "a();")).toBe(false);
  });

  test("idempotence: the fixed side still moves", () => {
    expect(isIdempotent(beforePath, before, options)).toBe(false);
  });

  test("program shape: the fixed side dropped a statement", () => {
    expect(shape("first();\nsecond();\n", "first();\n")).toBe(false);
    expect(shape("if (a) {\n  b();\n  ;\n}\n", "if (a) {\n  b();\n}\n")).toBe(false);
  });

  test("program shape: the fixed side no longer parses", () => {
    expect(shape("run();\n", "run(;\n")).toBe(false);
  });

  test("fixable left: the fixed side still carries a fixable finding", () => {
    expect(leavesNothingFixable(beforePath, before, options)).toBe(false);
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
  });

  test("compact braces, BigInt literals and a CRLF blank line pass", () => {
    for (const input of [
      "if (a) {b();}\n",
      "if(a){b();}\n",
      "if (a)  { b(); }\n",
      "if (a)\t{ b(); }\n",
    ])
      expect(judge("a.ts", input, false)).toMatchObject({ kind: "judged", broken: [] });

    expect(text("if (a) {\n  b(x);\n}\n", "if (a)\n  b(x) ;\n")).toBe(false);

    expect(text("a();\r\nb();\r\n", "a();\r\n\r\nb();\r\n")).toBe(true);
    expect(shape("const big = 10n;\n", "const big = 10n;\n")).toBe(true);
    expect(shape("const big = 10n;\n", "const big = 11n;\n")).toBe(false);
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
    return { exitCode: result.exitCode, stdout: result.stdout.toString() };
  }

  test("records compare across checkouts and changes fail the run", () => {
    const scratch = mkdtempSync(join(tmpdir(), "corpus-"));
    const first = join(scratch, "first");
    const second = join(scratch, "second");
    const record = join(scratch, "record.json");
    try {
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
    } finally {
      rmSync(scratch, { recursive: true, force: true });
    }
  });

  test("an unreadable file leaves the previous snapshot in place", () => {
    const scratch = mkdtempSync(join(tmpdir(), "corpus-"));
    const tree = join(scratch, "tree");
    const record = join(scratch, "record.json");
    const locked = join(tree, "nested.after.ts");
    try {
      cpSync(dir, tree, { recursive: true });
      expect(corpus(scratch, "--snapshot", record, "tree").exitCode).toBe(0);

      const baseline = readFileSync(record, "utf8");
      chmodSync(locked, 0o000);

      expect(corpus(scratch, "--snapshot", record, "tree").exitCode).toBe(2);
      expect(readFileSync(record, "utf8")).toBe(baseline);
    } finally {
      chmodSync(locked, 0o644);
      rmSync(scratch, { recursive: true, force: true });
    }
  });
});
