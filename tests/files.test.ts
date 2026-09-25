import { afterEach, describe, expect, test } from "bun:test";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { collectChanged, collectFiles, isCandidate, isGeneratedHeader } from "../src/files.ts";

const directories: string[] = [];

function directory(): string {
  const path = mkdtempSync(join(tmpdir(), "stanza-files-"));
  directories.push(path);
  return path;
}

function write(path: string, text = "export {};\n"): void {
  mkdirSync(resolve(path, ".."), { recursive: true });
  writeFileSync(path, text);
}

function git(cwd: string, ...args: string[]): void {
  const result = Bun.spawnSync(["git", "-C", cwd, ...args]);
  if (result.exitCode !== 0) throw new Error(new TextDecoder().decode(result.stderr));
}

function repository(): string {
  const cwd = directory();
  git(cwd, "init", "-q");

  git(cwd, "config", "user.email", "test@example.com");
  git(cwd, "config", "user.name", "Test User");
  git(cwd, "config", "commit.gpgsign", "false");

  return cwd;
}

afterEach(() => {
  for (const path of directories.splice(0))
    if (existsSync(path)) rmSync(path, { recursive: true, force: true });
});

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
  const cwd = directory();
  write(join(cwd, ".gitignore"), "/build\n*.log\ntmp/\n**/generated/**\n");
  write(join(cwd, "source/keep.ts"));

  write(join(cwd, "build/skip.ts"));
  write(join(cwd, "source/debug.log"));
  write(join(cwd, "tmp/skip.ts"));
  write(join(cwd, "source/generated/skip.ts"));
  write(join(cwd, "source/keep.txt"));

  expect(collectFiles([cwd], cwd)).toEqual({ files: [join(cwd, "source/keep.ts")], errors: [] });
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
    files: [join(realpathSync(cwd), "tracked.ts"), join(realpathSync(cwd), "untracked.ts")],
    errors: [],
  });
});

test("collects staged files before the first commit", () => {
  const cwd = repository();
  write(join(cwd, "staged.ts"));
  git(cwd, "add", "staged.ts");
  expect(collectChanged(cwd)).toEqual({
    files: [join(realpathSync(cwd), "staged.ts")],
    errors: [],
  });
});

test("a file named HEAD does not widen the changed set", () => {
  const cwd = repository();
  write(join(cwd, "tracked.ts"));
  git(cwd, "add", "tracked.ts");
  git(cwd, "commit", "-qm", "initial");

  write(join(cwd, "HEAD"), "x\n");
  expect(collectChanged(cwd)).toEqual({ files: [], errors: [] });
});

test("skipped directory names apply below the argument, not above it", () => {
  const cwd = join(directory(), "build", "project");
  write(join(cwd, "keep.ts"));
  write(join(cwd, "dist", "skip.ts"));
  expect(collectFiles([cwd], cwd)).toEqual({ files: [join(cwd, "keep.ts")], errors: [] });
  expect(collectFiles([join(cwd, "keep.ts")], cwd)).toEqual({
    files: [join(realpathSync(cwd), "keep.ts")],
    errors: [],
  });
});

test("a tracked symlink is skipped so fixes never write outside the repo", () => {
  const cwd = repository();
  const outside = join(directory(), "outside.ts");

  write(outside);
  write(join(cwd, "inside.ts"));
  symlinkSync(outside, join(cwd, "link.ts"));
  git(cwd, "add", "inside.ts", "link.ts");

  expect(collectFiles([cwd], cwd)).toEqual({
    files: [join(realpathSync(cwd), "inside.ts")],
    errors: [],
  });

  expect(collectChanged(cwd)).toEqual({
    files: [join(realpathSync(cwd), "inside.ts")],
    errors: [],
  });
});
