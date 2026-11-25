import { expect, test } from "bun:test";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { committedSource, git, hunkFunction, run, scratchGitRepository } from "./support.ts";

function stagedLines(cwd: string, ...flags: string[]): number[] {
  const result = run({ cwd }, "--check", "--staged", "--json", ...flags);
  expect(result.code).toBe(1);
  return JSON.parse(result.stdout).map((finding: { line: number }) => finding.line);
}

test("--staged --hunks reports only gaps and blocks the index changes", () => {
  const cwd = committedSource(`${hunkFunction("f1")}\n\n${hunkFunction("f2")}\n`);
  writeFileSync(join(cwd, "a.ts"), `${hunkFunction("f1")}\n\n${hunkFunction("f2", 3)}\n`);
  git(cwd, "add", "a.ts");
  writeFileSync(join(cwd, "a.ts"), `${hunkFunction("f1", 4)}\n\n${hunkFunction("f2", 3)}\n`);

  const scoped = stagedLines(cwd, "--hunks");
  expect(scoped.length).toBeGreaterThan(0);
  expect(scoped.every((line) => line > 7)).toBe(true);
  expect(stagedLines(cwd).some((line) => line < 7)).toBe(true);
  expect(run({ cwd }, "--check", "--staged", "--hunks").stderr).toBe(
    "1 finding in 1 staged file, fixable\nfix the staged lines of these by hand and restage them with git add -p, --fix would change the whole file: a.ts\n",
  );
});

test("--staged --hunks treats a staged new file as changed throughout", () => {
  const source = `${hunkFunction("f1")}\n\n${hunkFunction("f2")}\n`;
  const born = committedSource("export {};\n");
  writeFileSync(join(born, "b.ts"), source);
  git(born, "add", "b.ts");

  const unborn = scratchGitRepository({ files: { "b.ts": source }, staged: true });
  for (const cwd of [born, unborn]) {
    const lines = stagedLines(cwd, "--hunks");
    expect(lines.some((line) => line < 7)).toBe(true);
    expect(lines.some((line) => line > 7)).toBe(true);
    expect(lines).toEqual(stagedLines(cwd));
    expect(run({ cwd }, "--check", "--staged", "--hunks").stderr).toContain(
      "fix and restage with: ",
    );
  }
});

test("--staged --hunks checks a filtered file throughout", () => {
  const cwd = committedSource("export {};\n");
  git(cwd, "config", "filter.rot.clean", "tr a-zA-Z n-za-mN-ZA-M");
  git(cwd, "config", "filter.rot.smudge", "tr a-zA-Z n-za-mN-ZA-M");

  writeFileSync(join(cwd, ".gitattributes"), "b.ts filter=rot\n");
  writeFileSync(join(cwd, "b.ts"), `${hunkFunction("f1")}\n\n${hunkFunction("f2")}\n`);
  git(cwd, "add", ".gitattributes", "b.ts");
  git(cwd, "commit", "-qm", "rot");

  writeFileSync(join(cwd, "b.ts"), `${hunkFunction("f1")}\n\n${hunkFunction("f2", 3)}\n`);
  git(cwd, "add", "b.ts");

  expect(stagedLines(cwd, "--hunks").some((line) => line < 7)).toBe(true);
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
    "function f(a) {\n  first();\n\n  second();\n  if (a) {\n    third();\n  }\n}\n",
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
