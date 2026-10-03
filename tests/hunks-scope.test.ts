import { expect, test } from "bun:test";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { committedSource, gitBinary, hunkFunction, run, scratchGitRepository } from "./support.ts";

function stagedLines(cwd: string, ...flags: string[]): number[] {
  const result = run({ cwd }, "--check", "--staged", "--json", ...flags);
  expect(result.code).toBe(1);
  return JSON.parse(result.stdout).map((finding: { line: number }) => finding.line);
}

test("--staged --hunks reports only gaps and blocks the index changes", () => {
  const cwd = committedSource(`${hunkFunction("f1")}\n\n${hunkFunction("f2")}\n`);
  writeFileSync(join(cwd, "a.ts"), `${hunkFunction("f1")}\n\n${hunkFunction("f2", 3)}\n`);
  expect(Bun.spawnSync([gitBinary, "add", "a.ts"], { cwd }).exitCode).toBe(0);
  writeFileSync(join(cwd, "a.ts"), `${hunkFunction("f1", 4)}\n\n${hunkFunction("f2", 3)}\n`);

  const scoped = stagedLines(cwd, "--hunks");
  expect(scoped.length).toBeGreaterThan(0);
  expect(scoped.every((line) => line > 7)).toBe(true);
  expect(stagedLines(cwd).some((line) => line < 7)).toBe(true);
  expect(run({ cwd }, "--check", "--staged", "--hunks").stderr).toBe(
    "fix the staged lines of these by hand and restage them with git add -p, --fix would change the whole file: a.ts\n",
  );
});

test("--staged --hunks treats a staged new file as changed throughout", () => {
  const source = `${hunkFunction("f1")}\n\n${hunkFunction("f2")}\n`;
  const born = committedSource("export {};\n");
  writeFileSync(join(born, "b.ts"), source);
  expect(Bun.spawnSync([gitBinary, "add", "b.ts"], { cwd: born }).exitCode).toBe(0);

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
  const git = (...args: string[]) =>
    expect(Bun.spawnSync([gitBinary, ...args], { cwd }).exitCode).toBe(0);

  git("config", "filter.rot.clean", "tr a-zA-Z n-za-mN-ZA-M");
  git("config", "filter.rot.smudge", "tr a-zA-Z n-za-mN-ZA-M");
  writeFileSync(join(cwd, ".gitattributes"), "b.ts filter=rot\n");
  writeFileSync(join(cwd, "b.ts"), `${hunkFunction("f1")}\n\n${hunkFunction("f2")}\n`);
  git("add", ".gitattributes", "b.ts");
  git(
    "-c",
    "user.email=t@t",
    "-c",
    "user.name=t",
    "-c",
    "commit.gpgsign=false",
    "commit",
    "-qm",
    "rot",
  );

  writeFileSync(join(cwd, "b.ts"), `${hunkFunction("f1")}\n\n${hunkFunction("f2", 3)}\n`);
  git("add", "b.ts");

  expect(stagedLines(cwd, "--hunks").some((line) => line < 7)).toBe(true);
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

  const expected = "function f(a) {\n  if (a)\n    return 1;\n  return 2;\n}\n";
  for (let pass = 0; pass < 2; pass++) {
    const result = run({ cwd }, "--fix", "--changed", "--hunks");
    expect(result.code).toBe(0);
    expect(readFileSync(path, "utf8")).toBe(expected);
  }
});
