import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const cli = join(import.meta.dir, "..", "src", "cli.ts");

function run(...args: string[]): { code: number; stdout: string } {
  const result = Bun.spawnSync(["bun", "run", cli, ...args]);
  return { code: result.exitCode, stdout: new TextDecoder().decode(result.stdout) };
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
