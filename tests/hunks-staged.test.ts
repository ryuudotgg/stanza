import { expect, test } from "bun:test";
import { writeFileSync } from "node:fs";
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
