import { expect, test } from "bun:test";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
const fixture = join(root, "tests", "fixtures", "braces", "bodies.before.ts");

function rootWithoutBinary(): string {
  const dir = mkdtempSync(join(tmpdir(), "stanza-hooks-"));
  mkdirSync(join(dir, "git-hooks"));

  copyFileSync(join(root, "hook.sh"), join(dir, "hook.sh"));
  copyFileSync(join(root, "git-hooks", "pre-commit"), join(dir, "git-hooks", "pre-commit"));
  symlinkSync(join(root, "src"), join(dir, "src"));

  return dir;
}

function repository(): string {
  const dir = mkdtempSync(join(tmpdir(), "stanza-hooks-repo-"));
  Bun.spawnSync(["git", "init", "-q"], { cwd: dir });
  copyFileSync(fixture, join(dir, "a.ts"));
  Bun.spawnSync(["git", "add", "a.ts"], { cwd: dir });
  return dir;
}

function braces(text: string): number {
  return text.split("{").length - 1;
}

function hookEnv(flags = ""): Record<string, string | undefined> {
  return { ...process.env, AGENT_HOOKS: "1", STANZA_FLAGS: flags };
}

function preCommit(flags?: string): string {
  const cwd = repository();
  const result = Bun.spawnSync([join(rootWithoutBinary(), "git-hooks", "pre-commit")], {
    cwd,
    env: hookEnv(flags),
  });

  return new TextDecoder().decode(result.stdout);
}

function stopHook(flags?: string): string {
  const cwd = repository();
  Bun.spawnSync([join(rootWithoutBinary(), "hook.sh")], {
    cwd,
    env: hookEnv(flags),
    stdin: new TextEncoder().encode(JSON.stringify({ cwd })),
  });

  return readFileSync(join(cwd, "a.ts"), "utf8");
}

test("pre-commit forwards STANZA_FLAGS to stanza", () => {
  expect(preCommit()).toContain(" braces ");

  const output = preCommit("--no-braces");
  expect(output).not.toContain(" braces ");
  expect(output).toContain(" after-multiline ");
});

test("the Stop hook forwards STANZA_FLAGS to stanza", () => {
  const original = readFileSync(fixture, "utf8");
  expect(braces(stopHook())).toBeLessThan(braces(original));
  expect(braces(stopHook("--no-braces"))).toBe(braces(original));
});
