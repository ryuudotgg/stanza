import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import {
  isIdempotent,
  judge,
  leavesNothingFixable,
  preservesShape,
  preservesText,
  side,
} from "../scripts/corpus.ts";
import { bracesEnforced } from "../src/config.ts";

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
    expect(judge("a.ts", "if (a) {b();}\n", false)).toEqual({
      kind: "judged",
      output: "if (a) b();\n",
      broken: [],
    });

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
