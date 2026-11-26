import { expect, test } from "bun:test";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { cpuMs } from "./budget.ts";
import {
  committedSource,
  git,
  gitBinary,
  hunkFunction,
  run,
  scratchGitRepository,
} from "./support.ts";

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

  const fixedSecond = "function f2(a: boolean) {\n  if (a) return 1;\n  return 3;\n}";
  const expected = `${first}\n\n${fixedSecond}\n`;

  const fixed = run({ cwd }, "--fix", "--changed", "--hunks");
  expect(fixed.code).toBe(0);
  expect(fixed.stderr).toBe("rewrote 1 file, no findings left\n");
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
    .map((name) => `function ${name}(a: boolean) {\n  if (a) return 1;\n  return 2;\n}`)
    .join("\n\n");

  for (const staged of [false, true]) {
    const cwd = scratchGitRepository({ files: { "a.ts": source }, staged });
    const result = run({ cwd }, "--fix", "--changed", "--hunks");
    expect(result.code).toBe(0);
    expect(result.stderr).toBe("rewrote 1 file, no findings left\n");
    expect(readFileSync(join(cwd, "a.ts"), "utf8")).toBe(`${expected}\n`);
  }

  const cwd = committedSource("export {};\n");
  writeFileSync(join(cwd, "untracked.ts"), source);
  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  expect(readFileSync(join(cwd, "untracked.ts"), "utf8")).toBe(`${expected}\n`);
});

test("--hunks joins a short body through a touched gap with no blank line", () => {
  const source = "function f() {\n  if (a) {\n    first();\n  }\n  second();\n  last();\n}\n";

  const cwd = committedSource(source);
  const path = join(cwd, "a.ts");
  writeFileSync(path, source.replace("last();", "changed();"));

  const checked = run({ cwd }, "--check", "--changed", "--hunks", "--json");
  expect(checked.code).toBe(1);
  expect(
    JSON.parse(checked.stdout).map(
      ({ rule, line, col }: { rule: string; line: number; col: number }) => [rule, line, col],
    ),
  ).toEqual([["braces", 2, 10]]);

  const expected = "function f() {\n  if (a) first();\n  second();\n  changed();\n}\n";
  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(expected);
  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(expected);
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
    expect(result.stdout).not.toContain(": wall: ");
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
    const expected = `function f(a: number) {\n  g();\n  if (a) x(2)\n  if (b) y()\n${untouched}`;
    for (let pass = 0; pass < 3; pass++) {
      expect(run({ cwd }, "--fix", "--changed", "--hunks", "--braces").code).toBe(0);
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
      ": braces: ",
    );
  }

  expect(cpuMs() - started).toBeLessThan(5_000);
}, 60_000);

test("--hunks converges when a brace removal takes the blank lines inside the body", () => {
  const started = cpuMs();
  const counts = [0, 1, 2, 3];
  for (const [inside, outside] of counts.flatMap((inside) =>
    counts.map((outside) => [inside, outside]),
  )) {
    const elseBlank = "\n".repeat(inside!);
    const closeBlank = "\n".repeat(outside!);
    const committed = `function f(a, c) {\n  zero();\n  if (a) {\n    one();\n    two();\n  } else {\n${elseBlank}    call(c);\n  }\n${closeBlank}}\nfunction g() {}\n`;
    const expected = `function f(a, c) {\n  zero();\n  if (a) {\n    one(1);\n    two();\n  } else call(c);\n${closeBlank}}\n\nfunction g() {}\n`;

    const cwd = committedSource(committed);
    const path = join(cwd, "a.ts");
    writeFileSync(path, committed.replace("one();", "one(1);"));

    for (let pass = 0; pass < 2; pass++) {
      expect(run({ cwd }, "--fix", "--changed", "--hunks", "--braces").code).toBe(1);
      expect(readFileSync(path, "utf8")).toBe(expected);
    }

    const checked = run({ cwd }, "--check", "--changed", "--hunks", "--braces", "--json");
    expect(JSON.parse(checked.stdout)).toMatchObject([{ line: 3, col: 3, rule: "block-spacing" }]);
  }

  expect(cpuMs() - started).toBeLessThan(5_000);
}, 60_000);

test("--hunks counts an edit that drops braces stanza never removes", () => {
  const source = "function f(o) {\n  const { a } = o;\n  if (a) {\n    x()\n  }\n  y(a)\n}\n";
  const cwd = committedSource(source);
  writeFileSync(join(cwd, "a.ts"), source.replace("const { a } = o;", "const a = o;"));

  expect(run({ cwd }, "--check", "--changed", "--hunks", "--braces").stdout).toContain(
    ": braces: ",
  );
});

test("--hunks --check omits a blank line a later pass puts back", () => {
  const source = (guard: string) =>
    `function r(t) {\n  for (;;) {\n    if (${guard}) {\n      return null;\n    }\n    const u = t;\n    if (b) {\n      return u;\n    }\n  }\n}\n`;

  const cwd = committedSource(source("x"));
  const path = join(cwd, "a.ts");
  writeFileSync(path, source("a"));

  const checked = run({ cwd }, "--check", "--changed", "--hunks", "--braces").stdout;
  expect(checked.split("\n").map((line) => line.split(": ")[0])).toEqual([
    "a.ts:3:12",
    "a.ts:7:12",
    "",
  ]);

  expect(run({ cwd }, "--fix", "--changed", "--hunks", "--braces").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(
    "function r(t) {\n  for (;;) {\n    if (a) return null;\n    const u = t;\n    if (b) return u;\n  }\n}\n",
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

test("--hunks counts a body it joined onto its header as unchanged on the next run", () => {
  const source =
    "function f(a) {\n  const b = a + 1;\n  const c = b + 1;\n  if (c) {\n    return 1;\n  }\n  log(c);\n  return 2;\n}\n";

  const cwd = committedSource(source);
  const path = join(cwd, "a.ts");
  writeFileSync(path, source.replace("b + 1;", "b + 2;"));

  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  const fixed = readFileSync(path, "utf8");
  expect(fixed).toContain("  if (c) return 1;\n  log(c);\n");

  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(fixed);
  expect(run({ cwd }, "--check", "--changed", "--hunks").stdout).toBe("");
});

test("--hunks joins every guard of a short body it reaches through blank-led bodies", () => {
  const guard = (name: string, body: string) => `  if (${name}) {\n\n    ${body}\n  }\n`;
  const source = `function f() {\n${guard("a", "x(1);")}${guard("b", "y();")}${guard("c", "z();")}}\n`;

  const cwd = committedSource(source);
  const path = join(cwd, "a.ts");
  const changed = source.replace("x(1);", "x(2);");
  writeFileSync(path, changed);

  const expected = "function f() {\n  if (a) x(2);\n  if (b) y();\n  if (c) z();\n}\n";
  expect(run({ cwd }, "--fix", "--changed", "--hunks", "--braces").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(expected);

  expect(run({ cwd }, "--fix", "--changed", "--hunks", "--braces").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(expected);

  writeFileSync(path, changed);
  expect(run({ cwd }, "--fix", "--changed", "--braces").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(expected);
});

test("--hunks leaves the gap above a block its own brace removal shortened", () => {
  const source =
    "function f() {\n  const x = g();\n\n  if (x) {\n    return 1;\n  }\n  foo();\n  bar();\n}\n";

  const cwd = committedSource(source);
  const path = join(cwd, "a.ts");
  writeFileSync(path, source.replace("foo();", "edited();"));

  const expected =
    "function f() {\n  const x = g();\n\n  if (x) return 1;\n\n  edited();\n  bar();\n}\n";

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
  expect(result.stdout).toContain("a.ts:2:10: braces: ");
});

test("--changed --hunks checks a filtered file throughout", () => {
  const cwd = scratchGitRepository();
  const path = join(cwd, "a.ts");
  git(cwd, "config", "filter.banner.clean", "sed '/^\\/\\/ banner$/d'");
  git(cwd, "config", "filter.banner.smudge", "cat");
  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=banner\n");

  writeFileSync(path, `${"// banner\n".repeat(10)}${hunkFunction("f")}\n`);
  git(cwd, "add", ".");
  git(cwd, "commit", "-qm", "banner");
  writeFileSync(path, readFileSync(path, "utf8").replace("return 1;", "return 3;"));

  const scoped = run({ cwd }, "--check", "--changed", "--hunks", "--json");
  expect(scoped.code).toBe(1);
  expect(JSON.parse(scoped.stdout)).toEqual(
    JSON.parse(run({ cwd }, "--check", "--changed", "--json").stdout),
  );

  expect(JSON.parse(scoped.stdout)).toMatchObject([{ line: 12, rule: "braces" }]);
  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  const fixed = readFileSync(path, "utf8");
  expect(fixed.split("\n")[11]).toBe("  if (a) return 3;");

  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(fixed);
});

test("--hunks includes short-body dependencies and outer else-if owners", () => {
  const sources = [
    "function f(a: boolean) {\n  first();\n\n  second();\n  if (a) {\n    third();\n  }\n}\n",
    "function f(a, b) {\n  if (a) {\n    first();\n  } else if (b) {\n    second();\n  }\n  third();\n}\n",
  ];

  for (const [index, source] of sources.entries()) {
    const cwd = committedSource(source);
    const path = join(cwd, "a.ts");
    const changed = source.replace(index === 0 ? "first();" : "third();", "changed();");
    writeFileSync(path, changed);

    expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
    const scoped = readFileSync(path, "utf8");
    expect(scoped).not.toContain("if (a) {");
    expect(scoped).not.toContain("if (b) {");

    const clean = run({ cwd }, "--check", "--changed", "--hunks");
    expect([clean.code, clean.stdout]).toEqual([0, ""]);

    writeFileSync(path, changed);
    expect(run({ cwd }, "--fix", "--changed").code).toBe(0);
    expect(readFileSync(path, "utf8")).toBe(scoped);
  }
});

test("--hunks widens a touched gap only into a list short-body could join", () => {
  const topLevel = "if (a) {\n  g();\n}\nx = 1;\n\nh();\n";
  const multiline = "function f() {\n  if (a) {\n    g();\n  }\n  x = {\n    a,\n  };\n  h();\n}\n";

  const top = committedSource(topLevel);
  const topPath = join(top, "a.ts");
  const topChanged = topLevel.replace("h();", "changed();");
  writeFileSync(topPath, topChanged);

  const topCheck = run({ cwd: top }, "--check", "--changed", "--hunks", "--json");
  expect([topCheck.code, JSON.parse(topCheck.stdout)]).toEqual([0, []]);
  expect(run({ cwd: top }, "--fix", "--changed", "--hunks").code).toBe(0);
  expect(readFileSync(topPath, "utf8")).toBe(topChanged);

  const block = committedSource(multiline);
  const blockPath = join(block, "a.ts");
  writeFileSync(blockPath, multiline.replace("h();", "changed();"));

  const blockCheck = run({ cwd: block }, "--check", "--changed", "--hunks", "--json");
  expect(blockCheck.code).toBe(1);
  expect(JSON.parse(blockCheck.stdout)).toMatchObject([
    { line: 8, col: 3, rule: "after-multiline" },
  ]);

  expect(JSON.parse(blockCheck.stdout)).toHaveLength(1);

  expect(run({ cwd: block }, "--fix", "--changed", "--hunks").code).toBe(0);
  const fixed = readFileSync(blockPath, "utf8");
  expect(fixed).toBe(multiline.replace("  h();", "\n  changed();"));
  expect(run({ cwd: block }, "--fix", "--changed", "--hunks").code).toBe(0);
  expect(readFileSync(blockPath, "utf8")).toBe(fixed);

  const nested = [
    [
      "  second();\n  if (a) {\n    while (b) {\n      third();\n    }\n  }\n",
      "  second();\n  if (a) while (b) third();\n",
    ],
    [
      "  second();\n  if (a) {\n    consume(() => { if (b) {\n      third(); } });\n  }\n",
      "  second();\n  if (a) consume(() => { if (b) third(); });\n",
    ],
    [
      `  consume(() => { if (b) { ${"x".repeat(30)}(); } }); if (a) {\n    third();\n  }\n`,
      `  consume(() => { if (b) ${"x".repeat(30)}(); }); if (a) third();\n`,
    ],
  ];

  for (const [tail, joined] of nested) {
    const source = `function f(a, b) {\n  first();\n\n${tail}}\n`;
    const cwd = committedSource(source);
    const path = join(cwd, "a.ts");
    writeFileSync(path, source.replace("first();", "changed();"));

    expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
    const scoped = readFileSync(path, "utf8");
    expect(scoped).toBe(`function f(a, b) {\n  changed();\n${joined}}\n`);
    expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
    expect(readFileSync(path, "utf8")).toBe(scoped);
  }
});

test("--hunks scopes edge blanks independently", () => {
  const first = "function f1() {\n\n  first();\n\n}\n";
  const second = "function f2() {\n\n  first();\n  second();\n  third();\n  last();\n\n}\n";
  const cwd = committedSource(`${first}\n${second}`);
  const path = join(cwd, "a.ts");
  writeFileSync(path, `${first}\n${second.replace("first();", "changed();")}`);

  const checked = run({ cwd }, "--check", "--changed", "--hunks", "--json");
  expect(checked.code).toBe(1);
  expect(JSON.parse(checked.stdout).map((finding: { rule: string }) => finding.rule)).toEqual([
    "edge-blank",
  ]);

  expect(run({ cwd }, "--fix", "--changed", "--hunks").code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe(
    `${first}\n${second.replace("{\n\n", "{\n").replace("first();", "changed();")}`,
  );
});

test("--hunks reports a wall touched after its sixth statement through blank line edits", () => {
  const wall = (name: string) =>
    `function ${name}() {\n\n${Array.from({ length: 8 }, (_, index) => `  step${index}();\n`).join("")}\n}\n`;

  const first = wall("f1");
  const second = wall("f2");
  const cwd = committedSource(`${first}\n${second}`);
  const path = join(cwd, "a.ts");
  writeFileSync(path, `${first}\n${second.replace("step7();", "changed();")}`);

  for (const mode of ["--check", "--fix"]) {
    const result = run({ cwd }, mode, "--changed", "--hunks", "--json");
    expect(result.code).toBe(1);

    const walls = JSON.parse(result.stdout).filter(
      (finding: { rule: string }) => finding.rule === "wall",
    );

    expect(walls).toHaveLength(1);
    expect(walls[0].line).toBe(first.split("\n").length + 3);
  }

  expect(readFileSync(path, "utf8")).toStartWith(first);
});

test("--hunks keeps real separation outside scope when reporting walls", () => {
  const source =
    "function f(value) {\n  a();\n  b();\n  c();\n  const x = value;\n\n  if (x) done();\n  d();\n  e();\n  f();\n  last();\n}\n";

  const cwd = committedSource(source);
  const path = join(cwd, "a.ts");
  const changed = source.replace("last();", "changed();");
  writeFileSync(path, changed);

  for (const mode of ["--check", "--fix"]) {
    const result = run({ cwd }, mode, "--changed", "--hunks", "--json");
    expect(result.code).toBe(0);
    expect(JSON.parse(result.stdout)).toEqual([]);
  }

  expect(readFileSync(path, "utf8")).toBe(changed);
});

test("--hunks carries deleted changed blank lines into report-only findings", () => {
  const source =
    "function f(value) {\n  a();\n  b();\n  c();\n  const x = value;\n\n  if (x) done();\n  d();\n  e();\n}\n";

  const cwd = committedSource(source);
  const path = join(cwd, "a.ts");
  writeFileSync(path, source.replace("\n\n", "\n  \n"));

  const fixed = run({ cwd }, "--fix", "--changed", "--hunks", "--json");
  expect(fixed.code).toBe(1);
  expect(JSON.parse(fixed.stdout)).toEqual([
    expect.objectContaining({ line: 2, rule: "wall", fixable: false }),
  ]);

  expect(readFileSync(path, "utf8")).toBe(source.replace("\n\n", "\n"));
});

test("--hunks carries a deleted changed brace line into the next gap", () => {
  const source = "function f(a) {\n  if (a) {\n    return 1;\n  }\n\n  return 2;\n}\n";

  const cwd = committedSource(source);
  const path = join(cwd, "a.ts");
  writeFileSync(path, source.replace("  }\n", "  } \n"));

  const expected = "function f(a) {\n  if (a) return 1;\n  return 2;\n}\n";
  for (let pass = 0; pass < 2; pass++) {
    const result = run({ cwd }, "--fix", "--changed", "--hunks");
    expect(result.code).toBe(0);
    expect(readFileSync(path, "utf8")).toBe(expected);
  }
});
