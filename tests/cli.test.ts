import { expect, test } from "bun:test";
import {
  cpSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const cli = join(import.meta.dir, "..", "src", "cli.ts");

interface RunOptions {
  cwd?: string;
  env?: NodeJS.ProcessEnv;
}

function run(...input: (RunOptions | string)[]): {
  code: number;
  stderr: string;
  stdout: string;
} {
  const [first] = input;
  const options = typeof first === "object" ? first : {};
  const args = input.filter((item): item is string => typeof item === "string");
  const result = Bun.spawnSync(["bun", "run", cli, ...args], options);
  const decoder = new TextDecoder();
  return {
    code: result.exitCode,
    stderr: decoder.decode(result.stderr),
    stdout: decoder.decode(result.stdout),
  };
}

test("a file that is not UTF-8 is reported and left untouched", () => {
  const dir = mkdtempSync(join(tmpdir(), "stanza-cli-"));
  const file = join(dir, "latin1.ts");
  const bytes = Buffer.from('function f(a) {\n  if (a) {\n    log("caf\xe9");\n  }\n}\n', "latin1");
  writeFileSync(file, bytes);

  const result = run("--fix", file);
  expect(result.code).toBe(2);
  expect(result.stdout).toContain("parse not valid UTF-8");
  expect(readFileSync(file)).toEqual(bytes);
});

test("exit codes: clean 0, findings 1, usage 2", () => {
  const dir = mkdtempSync(join(tmpdir(), "stanza-cli-"));
  writeFileSync(join(dir, "clean.ts"), "export const a = 1;\n");
  writeFileSync(
    join(dir, "dirty.ts"),
    "export function f(a: boolean) {\n  if (a) {\n    return 1;\n  }\n  return 2;\n}\n",
  );

  expect(run("--check", join(dir, "clean.ts")).code).toBe(0);
  expect(run("--check", join(dir, "dirty.ts")).code).toBe(1);
  expect(run(join(dir, "dirty.ts")).code).toBe(2);
  expect(run("--fix", "--check", join(dir, "dirty.ts")).code).toBe(2);
});

test("--no-braces keeps braces and still reports blank line rules", () => {
  const fixture = join(import.meta.dir, "fixtures", "braces", "bodies.before.ts");
  const original = readFileSync(fixture, "utf8");
  const check = run("--check", "--no-braces", fixture);

  expect(check.code).toBe(1);
  expect(check.stdout.split("\n").some((line) => line.includes(" braces "))).toBe(false);
  expect(check.stdout).toContain(" after-multiline ");

  const dir = mkdtempSync(join(tmpdir(), "stanza-cli-"));
  const file = join(dir, "bodies.ts");
  writeFileSync(file, original);
  expect(run("--fix", "--no-braces", file).code).toBe(0);

  const fixed = readFileSync(file, "utf8");
  expect(fixed).not.toBe(original);
  expect(fixed.split("{").length).toBe(original.split("{").length);
  expect(run("--check", "--no-braces", file)).toEqual({ code: 0, stderr: "", stdout: "" });
  expect(run("--check", "--no-braces", "--no-braces", fixture).code).toBe(2);
});

test("falls back when git is absent", () => {
  const bin = mkdtempSync(join(tmpdir(), "stanza-no-git-"));
  const fixture = join(import.meta.dir, "fixtures");
  const copy = mkdtempSync(join(tmpdir(), "stanza-fixtures-"));
  const env = { ...process.env, PATH: bin };

  symlinkSync(process.execPath, join(bin, "bun"));
  cpSync(fixture, copy, { recursive: true });

  const withGit = run("--check", copy);
  const withoutGit = run({ env }, "--check", copy);

  expect(withoutGit.code).toBe(withGit.code);
  expect(withoutGit.stdout).toBe(withGit.stdout);
  expect(withGit.stderr).toBe("");
  expect(withoutGit.stderr.split("\n")).toEqual([expect.stringContaining("git"), ""]);

  const root = join(import.meta.dir, "..");
  const json = run({ cwd: root, env }, "--check", "--json", "src");

  expect(() => JSON.parse(json.stdout)).not.toThrow();
  expect(json.stderr.split("\n")).toEqual([expect.stringContaining("git"), ""]);

  const changed = run({ cwd: root, env }, "--check", "--changed");

  expect(changed.code).toBe(2);
  expect(changed.stderr.split("\n")).toEqual([expect.stringContaining("git"), ""]);
  expect(changed.stderr).not.toContain("at ");
});

test("a git failure while picking files exits 2", () => {
  const dir = mkdtempSync(join(tmpdir(), "stanza-cli-"));
  writeFileSync(join(dir, "a.ts"), "export const a = 1;\n");
  Bun.spawnSync(["git", "init", "-q"], { cwd: dir });
  Bun.spawnSync(["git", "add", "a.ts"], { cwd: dir });
  writeFileSync(join(dir, ".git", "index"), "junkjunkjunkjunkjunk");

  for (const args of [
    ["--check", "."],
    ["--check", "--changed"],
  ]) {
    const result = run({ cwd: dir }, ...args);
    expect(result.code).toBe(2);
    expect(result.stderr).toContain("git");
  }
});

test("git refusing an existing repository exits 2 instead of walking it", () => {
  const dir = mkdtempSync(join(tmpdir(), "stanza-cli-"));
  writeFileSync(join(dir, "a.ts"), "export const a = 1;\n");
  Bun.spawnSync(["git", "init", "-q"], { cwd: dir });

  const env = { ...process.env, GIT_TEST_ASSUME_DIFFERENT_OWNER: "1" };
  for (const args of [
    ["--check", "."],
    ["--check", "a.ts"],
    ["--check", "--changed"],
  ]) {
    const result = run({ cwd: dir, env }, ...args);
    expect(result.code).toBe(2);
    expect(result.stderr).toContain("dubious ownership");
  }
});

test("a repository hidden by GIT_CEILING_DIRECTORIES falls back to the walk", () => {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "stanza-cli-")));
  Bun.spawnSync(["git", "init", "-q"], { cwd: dir });
  mkdirSync(join(dir, "sub"));
  writeFileSync(
    join(dir, "sub", "dirty.ts"),
    "export function f(a: boolean) {\n  if (a) {\n    return 1;\n  }\n  return 2;\n}\n",
  );

  const env = { ...process.env, GIT_CEILING_DIRECTORIES: dir };
  const result = run({ cwd: join(dir, "sub"), env }, "--check", ".");
  expect(result.code).toBe(1);
  expect(result.stdout).toContain("dirty.ts");
});
