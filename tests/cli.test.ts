import { expect, test } from "bun:test";
import { cpSync, mkdtempSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const cli = join(import.meta.dir, "..", "src", "cli.ts");

interface RunOptions {
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  stdin?: Uint8Array;
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

test("--fix --stdin prints the fixed text and leaves the file alone", () => {
  const root = join(import.meta.dir, "..");
  const fixture = join(import.meta.dir, "fixtures", "braces", "bodies.before.ts");
  const original = readFileSync(fixture);
  const expected = readFileSync(join(import.meta.dir, "fixtures", "braces", "bodies.after.ts"));

  const copy = join(mkdtempSync(join(tmpdir(), "stanza-cli-")), "bodies.ts");
  writeFileSync(copy, original);

  const result = run(
    { cwd: root, stdin: original },
    "--fix",
    "--stdin",
    "tests/fixtures/braces/bodies.before.ts",
  );

  expect(result.stdout).toBe(expected.toString("utf8"));
  expect(result.code).toBe(run("--fix", copy).code);
  expect(readFileSync(fixture)).toEqual(original);
});

test("--check --stdin matches --check on the same file", () => {
  const dir = mkdtempSync(join(tmpdir(), "stanza-cli-"));
  const cases = [
    ["clean.ts", "export const a = 1;\n", 0],
    [
      "dirty.ts",
      "export function f(a: boolean) {\n  if (a) {\n    return 1;\n  }\n  return 2;\n}\n",
      1,
    ],
    ["broken.ts", "function (\n", 2],
  ] as const;

  for (const [name, text, code] of cases) {
    const file = join(dir, name);
    writeFileSync(file, text);

    const byPath = run("--check", file);
    const byStdin = run({ stdin: Buffer.from(text) }, "--check", "--stdin", file);

    expect(byPath.code).toBe(code);
    expect(byStdin.code).toBe(code);
    expect(byStdin.stdout).toBe(byPath.stdout);
  }
});

test("--fix --stdin echoes generated and unparsable input unchanged", () => {
  const dir = mkdtempSync(join(tmpdir(), "stanza-cli-"));
  const source =
    "export function f(a: boolean) {\n  if (a) {\n    return 1;\n  }\n  return 2;\n}\n";

  const generated = `// @generated\n${source}`;
  const broken = "function (\n";
  const fixed = run({ cwd: dir, stdin: Buffer.from(source) }, "--fix", "--stdin", "x.ts");

  expect(fixed.stdout).not.toBe(source);
  expect(run({ cwd: dir, stdin: Buffer.from(generated) }, "--fix", "--stdin", "x.ts")).toEqual({
    code: 0,
    stderr: "",
    stdout: generated,
  });

  const parse = run({ cwd: dir, stdin: Buffer.from(broken) }, "--fix", "--stdin", "x.ts");

  expect(parse.code).toBe(2);
  expect(parse.stdout).toBe(broken);
});

test("--stdin honours linguist-generated for a path whose directory does not exist", () => {
  const dir = mkdtempSync(join(tmpdir(), "stanza-cli-"));
  const source =
    "export function f(a: boolean) {\n  if (a) {\n    return 1;\n  }\n  return 2;\n}\n";

  Bun.spawnSync(["git", "init", "-q"], { cwd: dir });
  writeFileSync(join(dir, ".gitattributes"), "gen/** linguist-generated\n");

  expect(
    run({ cwd: dir, stdin: Buffer.from(source) }, "--fix", "--stdin", "gen/v2/api.ts"),
  ).toEqual({ code: 0, stderr: "", stdout: source });
});

test("--stdin with --changed, a positional path or no path is a usage error", () => {
  for (const args of [
    ["--fix", "--stdin", "x.ts", "--changed"],
    ["--fix", "--stdin", "x.ts", "y.ts"],
    ["--fix", "--stdin"],
  ]) {
    const result = run(...args);
    expect(result.code).toBe(2);
    expect(result.stdout).toBe("");
  }
});
