import { expect, test } from "bun:test";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { cpuMs } from "./budget.ts";
import { committedSource, gitBinary, hunkFunction, run, scratchGitRepository } from "./support.ts";

test("--hunks fixes and reports only the changed function", () => {
  const started = cpuMs();
  const first = hunkFunction("f1");
  const second = hunkFunction("f2", 3);
  const cwd = committedSource(`${first}\n\n${hunkFunction("f2")}\n`);

  const path = join(cwd, "a.ts");
  const changed = `${first}\n\n${second}\n`;
  writeFileSync(path, changed);

  const checked = run({ cwd }, "--check", "--changed", "--hunks", "--json");
  expect(checked.code).toBe(1);
  expect(checked.stderr).toBe("");

  const findings = JSON.parse(checked.stdout);
  expect(findings.length).toBeGreaterThan(0);
  expect(findings.every((finding: { line: number }) => finding.line > 7)).toBe(true);

  const fixedSecond = "function f2(a: boolean) {\n  if (a)\n    return 1;\n  return 3;\n}";
  const expected = `${first}\n\n${fixedSecond}\n`;

  const fixed = run({ cwd }, "--fix", "--changed", "--hunks");
  expect(fixed.code).toBe(0);
  expect(fixed.stderr).toBe("");
  expect(readFileSync(path, "utf8")).toBe(expected);

  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(expected);
  expect(run({ cwd }, "--check", "--changed", "--hunks").code).toBe(0);

  writeFileSync(path, changed);
  expect(run({ cwd }, "--fix", "--changed").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(
    `${fixedSecond.replace("f2", "f1").replace("return 3", "return 2")}\n\n${fixedSecond}\n`,
  );

  expect(cpuMs() - started).toBeLessThan(5_000);
}, 60_000);

test("--hunks fixes untracked files and files before the first commit throughout", () => {
  const source = `${hunkFunction("f1")}\n\n${hunkFunction("f2")}\n`;
  const expected = ["f1", "f2"]
    .map((name) => `function ${name}(a: boolean) {\n  if (a)\n    return 1;\n  return 2;\n}`)
    .join("\n\n");

  for (const staged of [false, true]) {
    const cwd = scratchGitRepository({ files: { "a.ts": source }, staged });
    const result = run({ cwd }, "--fix", "--changed", "--hunks");
    expect(result.code).toBe(0);
    expect(result.stderr).toBe("");
    expect(readFileSync(join(cwd, "a.ts"), "utf8")).toBe(`${expected}\n`);
  }

  const cwd = committedSource("export {};\n");
  writeFileSync(join(cwd, "untracked.ts"), source);
  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  expect(readFileSync(join(cwd, "untracked.ts"), "utf8")).toBe(`${expected}\n`);
});

test("--hunks finishes in one pass when a neighbour loses its braces", () => {
  const source =
    "function f(a: boolean) {\n  first();\n\n  second();\n  if (a) {\n    third();\n  }\n}\n";

  const cwd = committedSource(source);
  const path = join(cwd, "a.ts");
  const changed = source.replace("first();", "changed();");
  writeFileSync(path, changed);

  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  const scoped = readFileSync(path, "utf8");
  expect(run({ cwd }, "--check", "--changed", "--hunks").stdout).toBe("");

  writeFileSync(path, changed);
  expect(run({ cwd }, "--fix", "--changed").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(scoped);
});

test("--hunks keeps braces on a block away from a touched gap with no blank line", () => {
  const source =
    "function f(a: boolean) {\n  if (a) {\n    first();\n  }\n  second();\n  last();\n}\n";

  const cwd = committedSource(source);
  const path = join(cwd, "a.ts");
  const changed = source.replace("last();", "changed();");
  writeFileSync(path, changed);

  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(changed);
  expect(run({ cwd }, "--check", "--changed", "--hunks").stdout).toBe("");
});

test("--hunks keeps stanza-ignore as a wall boundary outside the hunk", () => {
  const calls = [
    "one();",
    "two();",
    "// stanza-ignore",
    "three();",
    "four();",
    "five();",
    "six();",
  ];

  const source = `function f() {\n${calls.map((call) => `  ${call}\n`).join("")}}\n`;
  const cwd = committedSource(source);
  writeFileSync(join(cwd, "a.ts"), source.replace("six();", "changed();"));

  for (const args of [[], ["--hunks"]]) {
    const result = run({ cwd }, "--check", "--changed", ...args);
    expect(result.stdout).not.toContain(" wall ");
  }
});

test("--hunks matches full --fix around brace removal in one pass", () => {
  const source =
    "function f(xs: number[]) {\n  outer: for (const x of xs) {\n    use(x);\n  }\n  done();\n  more();\n  last();\n}\n";

  const cwd = committedSource(source);
  const path = join(cwd, "a.ts");
  const changed = source.replace("done();", "edited();");
  writeFileSync(path, changed);

  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  const scoped = readFileSync(path, "utf8");
  expect(run({ cwd }, "--check", "--changed", "--hunks").stdout).toBe("");

  writeFileSync(path, changed);
  expect(run({ cwd }, "--fix", "--changed").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(scoped);
});

test("--hunks never spreads through the braces it removes, however many runs", () => {
  const started = cpuMs();

  for (const count of [3, 12]) {
    const blocks = Array.from(
      { length: count },
      (_, index) =>
        `  if (${"abcdefghijkl"[index]}) {\n    ${["x", "y", "z"][index] ?? `z${index}`}()\n  }\n`,
    );

    const source = `function f(a: number) {\n  g();\n${blocks.join("")}  w();\n}\n`;
    const cwd = committedSource(source);
    const path = join(cwd, "a.ts");
    writeFileSync(path, source.replace("x()", "x(2)"));

    const untouched = source.slice(source.indexOf("  if (c) {"));
    const expected = `function f(a: number) {\n  g();\n  if (a)\n    x(2)\n  if (b)\n    y()\n${untouched}`;
    for (let pass = 0; pass < 3; pass++) {
      expect(run({ cwd }, "--fix", "--changed", "--hunks", "--braces").code).toBe(1);
      expect(readFileSync(path, "utf8")).toBe(expected);
    }
  }

  expect(cpuMs() - started).toBeLessThan(5_000);
}, 60_000);

test("--hunks converges when repeated bodies or its own blank lines could realign", () => {
  const started = cpuMs();
  const guard = "  if (!NAME(v)) {\n    return null;\n  }\n";
  const guards = `function f(v) {\n  g(v);\n${["a", "b", "c", "d"].map((name) => guard.replace("NAME", name)).join("")}  return v;\n}\n`;
  const call = "function f() {\n  foo(\n    1,\n  );\n  g();\n  if (c) {\n    z()\n  }\n}\n";
  const inline = "function f() {\n  if (a) { x(); }\n\n  if (a) { x(); }\n\n  if (a) { x(); }\n}\n";
  const cases = [
    [guards, guards.replace("return null;", "return undefined;")],
    [call, call.replace("1,", "2,")],
    [inline, inline.replace("x();", "x(2);")],
  ];

  for (const [source, changed] of cases) {
    const cwd = committedSource(source!);
    const path = join(cwd, "a.ts");
    writeFileSync(path, changed!);

    run({ cwd }, "--fix", "--changed", "--hunks", "--braces");
    const fixed = readFileSync(path, "utf8");
    expect(fixed).not.toBe(changed);

    for (let pass = 0; pass < 2; pass++) {
      run({ cwd }, "--fix", "--changed", "--hunks", "--braces");
      expect(readFileSync(path, "utf8")).toBe(fixed);
    }

    expect(run({ cwd }, "--check", "--changed", "--hunks", "--braces").stdout).not.toContain(
      " braces ",
    );
  }

  expect(cpuMs() - started).toBeLessThan(5_000);
}, 60_000);

test("--hunks counts an edit that drops braces stanza never removes", () => {
  const source = "function f(o) {\n  const { a } = o;\n  if (a) {\n    x()\n  }\n  y(a)\n}\n";
  const cwd = committedSource(source);
  writeFileSync(join(cwd, "a.ts"), source.replace("const { a } = o;", "const a = o;"));

  expect(run({ cwd }, "--check", "--changed", "--hunks", "--braces").stdout).toContain(" braces ");
});

test("--hunks --check omits a blank line a later pass puts back", () => {
  const source = (guard: string) =>
    `function r(t) {\n  for (;;) {\n    if (${guard}) {\n      return null;\n    }\n    const u = t;\n    if (b) {\n      return u;\n    }\n  }\n}\n`;

  const cwd = committedSource(source("x"));
  const path = join(cwd, "a.ts");
  writeFileSync(path, source("a"));

  const checked = run({ cwd }, "--check", "--changed", "--hunks", "--braces").stdout;
  expect(checked.split("\n").map((line) => line.split(" ")[0])).toEqual([
    "a.ts:3:12",
    "a.ts:7:12",
    "",
  ]);

  expect(run({ cwd }, "--fix", "--changed", "--hunks", "--braces").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(
    "function r(t) {\n  for (;;) {\n    if (a)\n      return null;\n    const u = t;\n    if (b)\n      return u;\n  }\n}\n",
  );
});

test("--hunks converges after deleting a changed blank line", () => {
  const started = cpuMs();
  const cwd = committedSource("function f() {\n  a();\n\n  b();\n\n}\n");
  const path = join(cwd, "a.ts");
  writeFileSync(path, "function f() {\n  a();\n \n  b();\n\n}\n");

  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  const fixed = readFileSync(path, "utf8");
  expect(run({ cwd }, "--check", "--changed", "--hunks")).toMatchObject({ code: 0, stdout: "" });

  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(fixed);

  expect(cpuMs() - started).toBeLessThan(5_000);
}, 60_000);

test("--hunks leaves the gap above a block its own brace removal shortened", () => {
  const source =
    "function f() {\n  const x = g();\n\n  if (x) {\n    return 1;\n  }\n  foo();\n  bar();\n}\n";

  const cwd = committedSource(source);
  const path = join(cwd, "a.ts");
  writeFileSync(path, source.replace("foo();", "edited();"));

  const expected =
    "function f() {\n  const x = g();\n\n  if (x)\n    return 1;\n\n  edited();\n  bar();\n}\n";

  for (let pass = 0; pass < 2; pass++) {
    expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
    expect(readFileSync(path, "utf8")).toBe(expected);
  }

  expect(run({ cwd }, "--check", "--changed", "--hunks").stdout).toBe("");
});

test("--hunks leaves the next function alone when a changed brace line is removed", () => {
  const source =
    "function f(a: boolean) {\n  if (a) {\n    return 1;\n  }\n}\nfunction g() {\n\n  foo();\n}\n";

  const cwd = committedSource(source);
  const path = join(cwd, "a.ts");
  writeFileSync(path, source.replace("  }\n}", "  } \n}"));

  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  expect(readFileSync(path, "utf8")).toContain("function g() {\n\n  foo();");
});

test("--hunks treats a file removed from the index but left on disk as untracked", () => {
  const source = `${hunkFunction("f1")}\n`;
  const cwd = committedSource(source);
  expect(Bun.spawnSync([gitBinary, "mv", "a.ts", "b.ts"], { cwd }).exitCode).toBe(0);
  writeFileSync(join(cwd, "a.ts"), source);

  const result = run({ cwd }, "--check", "--changed", "--hunks");
  expect(result.stdout).toContain("a.ts:2:10 braces");
});

test("--hunks needs --changed or --staged", () => {
  const result = run("--fix", "--hunks", "a.ts");
  expect(result.code).toBe(2);
  expect(result.stderr).toContain("--hunks needs --changed or --staged");
});
