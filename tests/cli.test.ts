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

const gitRefusesOwnership = {
  ...process.env,
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_CONFIG_NOSYSTEM: "1",
  GIT_TEST_ASSUME_DIFFERENT_OWNER: "1",
};

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

  const env = { ...(options.env ?? process.env), FORCE_COLOR: undefined };
  const result = Bun.spawnSync(["bun", "run", cli, ...args], { ...options, env });

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

  for (const args of [
    ["--check", "."],
    ["--check", "a.ts"],
    ["--check", "--changed"],
  ]) {
    const result = run({ cwd: dir, env: gitRefusesOwnership }, ...args);
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

test("--fix --stdin keeps findings off stdout, as text and as --json", () => {
  const dir = mkdtempSync(join(tmpdir(), "stanza-cli-"));
  const wall = `export function f() {\n${"  step();\n".repeat(6)}}\n`;

  const text = run({ cwd: dir, stdin: Buffer.from(wall) }, "--fix", "--stdin", "wall.ts");

  expect(text.code).toBe(1);
  expect(text.stdout).toBe(wall);
  expect(text.stderr).toStartWith("wall.ts:2:3 wall ");

  const json = run({ cwd: dir, stdin: Buffer.from(wall) }, "--fix", "--json", "--stdin", "wall.ts");

  expect(json.code).toBe(1);
  expect(json.stdout).toBe(wall);
  expect(JSON.parse(json.stderr)).toEqual([
    expect.objectContaining({ path: "wall.ts", rule: "wall" }),
  ]);
});

test("--stdin takes the extension and config from the named path, not a symlink target", () => {
  const dir = mkdtempSync(join(tmpdir(), "stanza-cli-"));
  const other = mkdtempSync(join(tmpdir(), "stanza-cli-"));
  const typed =
    "export function f(a: number): number {\n  if (a) {\n    return 1;\n  }\n  return 2;\n}\n";

  writeFileSync(join(other, "view.js"), "");
  writeFileSync(
    join(other, "eslint.config.js"),
    'export default [{ rules: { curly: "error" } }];\n',
  );

  symlinkSync(join(other, "view.js"), join(dir, "view.ts"));

  const result = run({ cwd: dir, stdin: Buffer.from(typed) }, "--fix", "--stdin", "view.ts");

  expect(result.stdout).toBe(
    "export function f(a: number): number {\n  if (a)\n    return 1;\n  return 2;\n}\n",
  );

  expect(result.stderr).toBe("");
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

test("--fix --stdin in a repository git refuses echoes the input and exits 2", () => {
  const dir = mkdtempSync(join(tmpdir(), "stanza-cli-"));
  const source =
    "export function f(a: boolean) {\n  if (a) {\n    return 1;\n  }\n  return 2;\n}\n";

  Bun.spawnSync(["git", "init", "-q"], { cwd: dir });

  const result = run(
    { cwd: dir, env: gitRefusesOwnership, stdin: Buffer.from(source) },
    "--fix",
    "--stdin",
    "x.ts",
  );

  expect(result.code).toBe(2);
  expect(result.stdout).toBe(source);
  expect(result.stderr).toContain("dubious ownership");
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

test("join findings name the binding and the statement that reads it", () => {
  const dir = mkdtempSync(join(tmpdir(), "stanza-cli-"));
  const file = join(dir, "joins.ts");
  writeFileSync(
    file,
    [
      "export function f(rows: number[]) {",
      "  const [head, tail] = split(rows);",
      "",
      "  if (tail) return head;",
      "",
      "  let total = 0;",
      "",
      "  for (const row of rows) total += row;",
      "  const kind = classify(total);",
      "",
      "  return kind;",
      "}",
      "",
    ].join("\n"),
  );

  const { stdout } = run("--check", file);
  expect(stdout).toContain("4:3 guard-join keep `tail` next to the `if` that reads it");
  expect(stdout).toContain("8:3 use-join keep `total` next to the `for` that reads it");
  expect(stdout).toContain("11:3 consume-join keep `kind` next to the `return` that reads it");
});
