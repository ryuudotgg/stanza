import { expect, test } from "bun:test";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { gitIn, run, scratch, scratchGitRepository } from "./support.ts";

const fixable = readFileSync(
  join(import.meta.dir, "fixtures", "after-guard", "example.before.ts"),
  "utf8",
);

const byHand = `export function f() {\n${"  step();\n".repeat(8)}}\n`;
const clean = "export const a = 1;\n";
const broken = "const = ;\n";
const generated = `// @generated\n${fixable}`;

function folder(files: Record<string, string>): string {
  const cwd = scratch("summary");
  for (const [name, text] of Object.entries(files)) {
    mkdirSync(join(cwd, name, ".."), { recursive: true });
    writeFileSync(join(cwd, name), text);
  }

  return cwd;
}

function summary(cwd: string, ...args: string[]): { code: number; stderr: string } {
  const { code, stderr } = run({ cwd }, ...args);
  return { code, stderr };
}

function committed(files: Record<string, string>): string {
  const cwd = scratchGitRepository({ files, staged: true });
  gitIn(cwd, "commit", "-qm", "init");
  return cwd;
}

test("a check over paths ends with the findings it counted", () => {
  const cases: [Record<string, string>, number, string][] = [
    [
      { "src/a.ts": fixable, "src/b.ts": byHand },
      1,
      "3 findings in 2 files: 2 fixable with --fix, 1 by hand",
    ],
    [{ "src/a.ts": fixable }, 1, "2 findings in 1 file, all fixable with --fix"],
    [{ "src/b.ts": byHand }, 1, "1 finding in 1 file, by hand"],
    [{ "src/a.ts": clean, "src/b.ts": clean }, 0, "checked 2 files, no findings"],
    [
      { "src/a.ts": broken, "src/b.ts": clean, "src/c.ts": clean },
      2,
      "checked 3 files, no findings, 1 file failed",
    ],
  ];

  for (const [files, code, line] of cases)
    expect(summary(folder(files), "--check", "src")).toEqual({ code, stderr: `${line}\n` });
});

test("a failed file joins the findings clause and every file counts once", () => {
  const cwd = folder({ "src/a.ts": fixable, "src/b.ts": broken, "src/c.ts": broken });
  expect(summary(cwd, "--check", "src")).toEqual({
    code: 2,
    stderr: "2 findings in 1 file, all fixable with --fix, 2 files failed\n",
  });
});

test("a fix over paths ends with what it rewrote and what it left", () => {
  const cases: [Record<string, string>, number, string][] = [
    [
      { "src/a.ts": fixable, "src/b.ts": byHand },
      1,
      "rewrote 1 file, 1 finding left to fix by hand",
    ],
    [{ "src/a.ts": fixable, "src/c.ts": fixable }, 0, "rewrote 2 files, no findings left"],
    [{ "src/b.ts": byHand }, 1, "checked 1 file, 1 finding left to fix by hand"],
    [{ "src/a.ts": clean }, 0, "checked 1 file, nothing to fix"],
  ];

  for (const [files, code, line] of cases)
    expect(summary(folder(files), "--fix", "src")).toEqual({ code, stderr: `${line}\n` });
});

test("a path that fails to select leaves the summary counting the files that ran", () => {
  const cwd = folder({ "src/a.ts": clean });
  expect(summary(cwd, "--check", "src", "missing.ts")).toEqual({
    code: 2,
    stderr: "stanza: no such file: missing.ts\nchecked 1 file, no findings\n",
  });
});

test("generated files are left out of the count", () => {
  const cwd = folder({ "src/a.ts": clean, "src/gen.ts": generated });
  expect(summary(cwd, "--check", "src").stderr).toBe("checked 1 file, no findings\n");
});

test("a run that formats no file prints no summary", () => {
  const cwd = folder({ "gen/a.ts": generated });
  expect(summary(cwd, "--check", "gen")).toEqual({
    code: 0,
    stderr: "stanza: nothing to format in gen, generated files are skipped\n",
  });

  expect(summary(cwd, "--check", "missing.ts")).toEqual({
    code: 2,
    stderr: "stanza: no such file: missing.ts\n",
  });
});

test("--json, --stdin and the stdin fix print no summary", () => {
  const cwd = folder({ "src/a.ts": fixable });
  expect(summary(cwd, "--check", "--json", "src").stderr).toBe("");
  expect(summary(cwd, "--fix", "--json", "src").stderr).toBe("");

  const stdin = Buffer.from(fixable);
  expect(run({ cwd, stdin }, "--check", "--stdin", "a.ts").stderr).toBe("");
  expect(run({ cwd, stdin }, "--fix", "--stdin", "a.ts").stderr).toBe("");
});

test("--changed with no changes says so under --check and --fix", () => {
  const cwd = committed({ "a.ts": fixable });
  expect(summary(cwd, "--check", "--changed")).toEqual({
    code: 0,
    stderr: "no changed files to check\n",
  });

  expect(summary(cwd, "--fix", "--changed")).toEqual({
    code: 0,
    stderr: "no changed files to fix\n",
  });
});

test("--changed counts the changed files it checked and fixed", () => {
  const cwd = committed({ "a.ts": clean, "b.ts": clean, "c.ts": clean });
  writeFileSync(join(cwd, "a.ts"), fixable);
  writeFileSync(join(cwd, "b.ts"), `${clean}${clean}`);

  expect(summary(cwd, "--check", "--changed")).toEqual({
    code: 1,
    stderr: "2 findings in 1 file, all fixable with --fix\n",
  });

  expect(summary(cwd, "--fix", "--changed")).toEqual({
    code: 0,
    stderr: "rewrote 1 file, no findings left\n",
  });

  expect(summary(cwd, "--check", "--changed")).toEqual({
    code: 0,
    stderr: "checked 2 files, no findings\n",
  });
});

test("--staged ends with the summary, then the repair lines", () => {
  const cwd = scratchGitRepository({
    files: { "a.ts": fixable, "b.ts": byHand, "c.ts": clean },
    staged: true,
  });

  expect(summary(cwd, "--check", "--staged")).toEqual({
    code: 1,
    stderr: [
      "3 findings in 2 staged files: 2 fixable, 1 by hand",
      `fix and restage with: cd ${cwd} && stanza --fix -- a.ts && git --literal-pathspecs add -- a.ts`,
      "",
    ].join("\n"),
  });
});

test("--staged counts a clean commit and says when nothing is staged", () => {
  const cwd = scratchGitRepository({ files: { "a.ts": clean, "b.ts": clean }, staged: true });
  expect(summary(cwd, "--check", "--staged")).toEqual({
    code: 0,
    stderr: "checked 2 staged files, no findings\n",
  });

  const empty = scratchGitRepository({ files: { "a.ts": fixable } });
  expect(summary(empty, "--check", "--staged")).toEqual({
    code: 0,
    stderr: "no staged files to check\n",
  });
});

test("--staged prints notices before the summary and the repair lines after it", () => {
  const cwd = scratchGitRepository({
    files: { ".oxfmtrc.json": "{", "a.ts": fixable },
    staged: true,
  });

  expect(summary(cwd, "--check", "--staged")).toEqual({
    code: 1,
    stderr: [
      "stanza: cannot read the line width from .oxfmtrc.json, so the next source sets it",
      "2 findings in 1 staged file, all fixable",
      `fix and restage with: cd ${cwd} && stanza --fix -- a.ts && git --literal-pathspecs add -- a.ts`,
      "",
    ].join("\n"),
  });
});
