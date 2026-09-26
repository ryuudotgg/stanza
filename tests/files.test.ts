import { describe, expect, test } from "bun:test";
import { mkdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { collectChanged, collectFiles, isCandidate, isGeneratedHeader } from "../src/files.ts";
import { scratch, scratchGitRepository } from "./support.ts";

function write(path: string, text = "export {};\n"): void {
  mkdirSync(resolve(path, ".."), { recursive: true });
  writeFileSync(path, text);
}

function git(cwd: string, ...args: string[]): void {
  const result = Bun.spawnSync(["git", "-C", cwd, ...args]);
  if (result.exitCode !== 0) throw new Error(new TextDecoder().decode(result.stderr));
}

function repository(): string {
  const cwd = scratchGitRepository();

  git(cwd, "config", "user.email", "test@example.com");
  git(cwd, "config", "user.name", "Test User");
  git(cwd, "config", "commit.gpgsign", "false");

  return cwd;
}

describe("isCandidate", () => {
  test("accepts supported extensions and skips generated paths", () => {
    const accepted = [
      "source/file.ts",
      "source/file.tsx",
      "source/file.mts",
      "source/file.cts",
      "source/file.js",
      "source/file.jsx",
      "source/file.mjs",
      "source/file.cjs",
    ];

    const rejected = [
      "source/file.d.ts",
      "source/file.gen.ts",
      "source/file.generated.ts",
      "source/file.min.js",
      "node_modules/file.ts",
      "source/file.txt",
    ];

    for (const path of accepted) expect(isCandidate(path)).toBe(true);

    for (const path of rejected) expect(isCandidate(path)).toBe(false);
  });

  test("recognizes generated headers", () => {
    expect(isGeneratedHeader("// @generated\nexport {};\n")).toBe(true);
    expect(isGeneratedHeader("// DO NOT EDIT\nexport {};\n")).toBe(true);
    expect(isGeneratedHeader("// automatically generated\nexport {};\n")).toBe(true);
    expect(isGeneratedHeader("export {};\n")).toBe(false);
  });
});

test("walks non git directories with the root ignore file", () => {
  const cwd = scratch("files");
  write(join(cwd, ".gitignore"), "/build\n*.log\ntmp/\n**/generated/**\n");
  write(join(cwd, "source/keep.ts"));

  write(join(cwd, "build/skip.ts"));
  write(join(cwd, "source/debug.log"));
  write(join(cwd, "tmp/skip.ts"));
  write(join(cwd, "source/generated/skip.ts"));
  write(join(cwd, "source/keep.txt"));

  expect(collectFiles([cwd], cwd)).toEqual({
    files: [join(cwd, "source/keep.ts")],
    errors: [],
    warnings: [],
  });
});

test("uses git to include tracked and untracked files but not ignored files", () => {
  const cwd = repository();
  write(join(cwd, ".gitignore"), "ignored/\n");
  write(join(cwd, "tracked.ts"));
  git(cwd, "add", ".gitignore", "tracked.ts");
  git(cwd, "commit", "-qm", "initial");

  write(join(cwd, "untracked.ts"));
  write(join(cwd, "ignored/skip.ts"));

  expect(collectFiles([cwd], cwd)).toEqual({
    files: [join(realpathSync(cwd), "tracked.ts"), join(realpathSync(cwd), "untracked.ts")],
    errors: [],
    warnings: [],
  });
});

test("collects modified and untracked files", () => {
  const cwd = repository();
  write(join(cwd, "tracked.ts"));
  git(cwd, "add", "tracked.ts");
  git(cwd, "commit", "-qm", "initial");

  write(join(cwd, "tracked.ts"), "export const changed = true;\n");
  write(join(cwd, "untracked.ts"));

  expect(collectChanged(cwd)).toEqual({
    changedLines: new Map(),
    files: [join(realpathSync(cwd), "tracked.ts"), join(realpathSync(cwd), "untracked.ts")],
    errors: [],
    warnings: [],
  });
});

test("collects staged files before the first commit", () => {
  const cwd = repository();
  write(join(cwd, "staged.ts"));
  git(cwd, "add", "staged.ts");
  expect(collectChanged(cwd)).toEqual({
    changedLines: new Map(),
    files: [join(realpathSync(cwd), "staged.ts")],
    errors: [],
    warnings: [],
  });
});

test("hunks keep zero context despite git config, environment and binary attributes", () => {
  const source = Array.from({ length: 20 }, (_, index) => `const n${index} = ${index};\n`).join("");
  const cwd = scratchGitRepository({
    files: { "a.ts": source, "binary.ts": source, ".gitattributes": "binary.ts binary\n" },
    staged: true,
  });

  git(cwd, "config", "commit.gpgsign", "false");
  git(cwd, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init");
  git(cwd, "config", "diff.interHunkContext", "10");

  const changed = source.replace("n2 = 2", "n2 = 30").replace("n8 = 8", "n8 = 90");
  write(join(cwd, "a.ts"), changed);
  write(join(cwd, "binary.ts"), changed);
  write(join(cwd, "untracked.ts"), changed);

  const previous = process.env.GIT_DIFF_OPTS;
  process.env.GIT_DIFF_OPTS = "--unified=10";

  try {
    const collected = collectChanged(cwd, undefined, true);
    expect(collected.errors).toEqual([]);
    expect(collected.changedLines).toEqual(
      new Map([
        [join(realpathSync(cwd), "a.ts"), new Set([3, 9])],
        [join(realpathSync(cwd), "binary.ts"), new Set([3, 9])],
      ]),
    );
  } finally {
    if (previous === undefined) delete process.env.GIT_DIFF_OPTS;
    else process.env.GIT_DIFF_OPTS = previous;
  }
});

test("hunks map quoted paths and mark both sides of deletions", () => {
  const cwd = scratchGitRepository({
    files: { 'a "quoted".ts': "a();\nb();\nc();\n", "z.ts": "x();\n" },
    staged: true,
  });

  git(cwd, "config", "commit.gpgsign", "false");
  git(cwd, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init");
  write(join(cwd, 'a "quoted".ts'), "a();\nc();\n");
  write(join(cwd, "z.ts"), "y();\n");

  const collected = collectChanged(cwd, undefined, true);
  expect(collected.errors).toEqual([]);
  expect(collected.changedLines).toEqual(
    new Map([
      [join(realpathSync(cwd), 'a "quoted".ts'), new Set([1, 2])],
      [join(realpathSync(cwd), "z.ts"), new Set([1])],
    ]),
  );
});

test("hunks merge typechange sections before matching file names", () => {
  const cwd = scratchGitRepository({ files: { "z.ts": "z();\n" }, staged: true });
  symlinkSync("z.ts", join(cwd, "a.ts"));
  git(cwd, "add", "a.ts");

  git(cwd, "config", "commit.gpgsign", "false");
  git(cwd, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init");

  rmSync(join(cwd, "a.ts"));
  write(join(cwd, "a.ts"), "a();\nb();\n");
  write(join(cwd, "z.ts"), "y();\n");

  const collected = collectChanged(cwd, undefined, true);
  expect(collected.errors).toEqual([]);
  expect(collected.changedLines.get(join(realpathSync(cwd), "a.ts"))).toEqual(new Set([1, 2]));
  expect(collected.changedLines.get(join(realpathSync(cwd), "z.ts"))).toEqual(new Set([1]));
});

test("a file named HEAD does not widen the changed set", () => {
  const cwd = repository();
  write(join(cwd, "tracked.ts"));
  git(cwd, "add", "tracked.ts");
  git(cwd, "commit", "-qm", "initial");

  write(join(cwd, "HEAD"), "x\n");
  expect(collectChanged(cwd)).toEqual({
    changedLines: new Map(),
    files: [],
    errors: [],
    warnings: [],
  });
});

test("skipped directory names apply below the argument, not above it", () => {
  const cwd = join(scratch("files"), "build", "project");
  write(join(cwd, "keep.ts"));
  write(join(cwd, "dist", "skip.ts"));
  expect(collectFiles([cwd], cwd)).toEqual({
    files: [join(cwd, "keep.ts")],
    errors: [],
    warnings: [],
  });

  expect(collectFiles([join(cwd, "keep.ts")], cwd)).toEqual({
    files: [join(realpathSync(cwd), "keep.ts")],
    errors: [],
    warnings: [],
  });
});

test("a tracked symlink is skipped so fixes never write outside the repo", () => {
  const cwd = repository();
  const outside = join(scratch("files"), "outside.ts");

  write(outside);
  write(join(cwd, "inside.ts"));
  symlinkSync(outside, join(cwd, "link.ts"));
  git(cwd, "add", "inside.ts", "link.ts");

  expect(collectFiles([cwd], cwd)).toEqual({
    files: [join(realpathSync(cwd), "inside.ts")],
    errors: [],
    warnings: [],
  });

  expect(collectChanged(cwd)).toEqual({
    changedLines: new Map(),
    files: [join(realpathSync(cwd), "inside.ts")],
    errors: [],
    warnings: [],
  });
});

test("a tracked directory replaced by a symlink out of the repo is skipped", () => {
  const cwd = repository();
  const elsewhere = scratch("files");

  write(join(cwd, "keep.ts"));
  write(join(cwd, "pkg/moved.ts"));
  git(cwd, "add", "keep.ts", "pkg/moved.ts");

  rmSync(join(cwd, "pkg"), { recursive: true });
  write(join(elsewhere, "moved.ts"));
  symlinkSync(elsewhere, join(cwd, "pkg"));

  expect(collectFiles([cwd], cwd)).toEqual({
    files: [join(realpathSync(cwd), "keep.ts")],
    errors: [],
    warnings: [],
  });
});

test("a broken branch ref is an error, an orphan branch is not", () => {
  const cwd = repository();
  write(join(cwd, "tracked.ts"));
  git(cwd, "add", "tracked.ts");
  git(cwd, "commit", "-q", "-m", "initial");

  git(cwd, "checkout", "-q", "--orphan", "fresh");
  expect(collectChanged(cwd)).toEqual({
    changedLines: new Map(),
    files: [join(realpathSync(cwd), "tracked.ts")],
    errors: [],
    warnings: [],
  });

  writeFileSync(join(cwd, ".git", "refs", "heads", "fresh"), "junk\n");
  const broken = collectChanged(cwd);
  expect(broken.files).toEqual([]);
  expect(broken.errors).toEqual([expect.stringContaining("git symbolic-ref failed")]);
});

test("a broken git index is an error, not an empty selection", () => {
  const cwd = repository();
  write(join(cwd, "tracked.ts"));
  git(cwd, "add", "tracked.ts");
  writeFileSync(join(cwd, ".git", "index"), "junkjunkjunkjunkjunk");

  for (const collected of [collectFiles(["."], cwd), collectChanged(cwd)]) {
    expect(collected.files).toEqual([]);
    expect(collected.errors.length).toBeGreaterThan(0);
    for (const error of collected.errors) expect(error).toContain("git");
  }
});

test("explicit files are collected across repositories and outside them", () => {
  const first = repository();
  const second = repository();
  const outside = scratch("files");

  write(join(first, ".gitattributes"), "schema.ts linguist-generated\n");
  write(join(first, "keep.ts"));
  write(join(first, "schema.ts"));
  write(join(second, "nested/keep.ts"));
  write(join(outside, "keep.ts"));

  const inputs = [
    join(first, "keep.ts"),
    join(first, "schema.ts"),
    join(second, "nested/keep.ts"),
    join(outside, "keep.ts"),
  ];

  const expected = [
    join(realpathSync(first), "keep.ts"),
    join(realpathSync(second), "nested/keep.ts"),
    join(realpathSync(outside), "keep.ts"),
  ].sort((left, right) => left.localeCompare(right));

  expect(collectFiles(inputs, outside)).toEqual({ files: expected, errors: [], warnings: [] });
});
