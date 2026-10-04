import { expect, test } from "bun:test";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { usage } from "../src/usage.ts";
import { run, scratch, spawnCli } from "./support.ts";

test("explain accepts a leading dash location after --", () => {
  const cwd = scratch("explain-dashdash");
  writeFileSync(join(cwd, "-dash.ts"), "if (ready) {\n  work();\n}\n");

  const expected = spawnCli({ cwd }, "explain", "./-dash.ts:1");
  expect(expected.code).toBe(0);
  expect(spawnCli({ cwd }, "explain", "--", "-dash.ts:1")).toEqual(expected);
});

test.each([
  [[], "explain needs <file>:<line>"],
  [["a.ts:1", "b.ts:2"], "unexpected argument b.ts:2"],
  [["--no-braces"], "explain needs <file>:<line>"],
  [["a.ts:1", "--no-braces"], "unexpected argument --no-braces"],
  [["--", "a.ts:1"], "unexpected argument a.ts:1"],
  [["a.ts:0"], "a.ts:0: the line must be 1 or more"],
])("explain rejects invalid locations after --: %j", (locations, problem) => {
  expect(run("explain", "--", ...locations)).toEqual({
    code: 2,
    stderr: `stanza: ${problem}\n${usage}\n`,
    stdout: "",
  });
});

test("explain recognizes --no-braces only before --", () => {
  const cwd = scratch("explain-no-braces");
  writeFileSync(join(cwd, "--no-braces.ts"), "if (ready) {\n  work();\n}\n");
  writeFileSync(join(cwd, "-dash.ts"), "if (ready) {\n  work();\n}\n");

  const expected = run({ cwd }, "explain", "./-dash.ts:1", "--no-braces");
  expect(expected.code).toBe(0);
  expect(expected.stdout).toContain("--no-braces");
  expect(run({ cwd }, "explain", "--no-braces", "--", "-dash.ts:1")).toEqual(expected);
  expect(run({ cwd }, "explain", "--", "--no-braces.ts:1")).toEqual(
    run({ cwd }, "explain", "./--no-braces.ts:1"),
  );
});

test.each([
  [["--no-braces", "--no-braces", "a.ts:1"], "--no-braces given twice"],
  [["-dash.ts:1"], "unexpected argument -dash.ts:1"],
  [["a.ts:1", "b.ts:2"], "unexpected argument b.ts:2"],
])("explain preserves usage errors without --: %j", (args, problem) => {
  expect(run("explain", ...args)).toEqual({
    code: 2,
    stderr: `stanza: ${problem}\n${usage}\n`,
    stdout: "",
  });
});

test("explain preserves exit 1 after -- when nothing is explained", () => {
  const cwd = scratch("explain-no-gap");
  writeFileSync(join(cwd, "-dash.ts"), "work();\n");

  const expected = run({ cwd }, "explain", "./-dash.ts:1");
  expect(expected.code).toBe(1);
  expect(run({ cwd }, "explain", "--", "-dash.ts:1")).toEqual(expected);
});

test("explain usage shows the delimiter after options", () => {
  expect(run("--help").stdout).toContain("stanza explain [--no-braces] [--] <file>:<line>");
});
