import { expect, test } from "bun:test";
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
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

function repository(
  files: Record<string, string> = { "a.ts": readFileSync(fixture, "utf8") },
): string {
  const dir = mkdtempSync(join(tmpdir(), "stanza-hooks-repo-"));
  Bun.spawnSync(["git", "init", "-q"], { cwd: dir });

  for (const [path, text] of Object.entries(files)) {
    mkdirSync(join(dir, path, ".."), { recursive: true });
    writeFileSync(join(dir, path), text);
  }

  const staged = Object.keys(files).filter((path) => !path.startsWith("node_modules/"));
  Bun.spawnSync(["git", "add", ...staged], { cwd: dir });
  return dir;
}

function braces(text: string): number {
  return text.split("{").length - 1;
}

function hookEnv(flags = ""): Record<string, string | undefined> {
  return { ...process.env, AGENT_HOOKS: "1", STANZA_CONFIG_ROOT: undefined, STANZA_FLAGS: flags };
}

function preCommit(
  files?: Record<string, string>,
  flags?: string,
): ReturnType<typeof Bun.spawnSync> {
  const cwd = repository(files);
  const result = Bun.spawnSync([join(rootWithoutBinary(), "git-hooks", "pre-commit")], {
    cwd,
    env: hookEnv(flags),
  });

  return result;
}

function output(result: ReturnType<typeof Bun.spawnSync>): string {
  return new TextDecoder().decode(result.stdout);
}

function rules(findings: string, path: string): string[] {
  return findings
    .split("\n")
    .filter((line) => line.startsWith(`${path}:`))
    .map((line) => line.split(" ")[1] ?? "");
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
  expect(output(preCommit())).toContain(" braces ");

  const findings = output(preCommit(undefined, "--no-braces"));
  expect(findings).not.toContain(" braces ");
  expect(findings).toContain(" after-multiline ");
});

test("pre-commit resolves shared ESLint packages from the repository", () => {
  const findings = output(
    preCommit({
      ".eslintrc.json": '{ "extends": ["@acme/eslint-config"] }',
      "node_modules/@acme/eslint-config/package.json":
        '{"name":"@acme/eslint-config","main":"index.json"}',
      "node_modules/@acme/eslint-config/index.json": '{ "rules": { "curly": "off" } }',
      "a.ts": readFileSync(fixture, "utf8"),
    }),
  );

  expect(findings).toContain(" braces ");
});

test("pre-commit resolves nested ESLint config from the repository", () => {
  const findings = output(
    preCommit({
      ".eslintrc.json": '{ "rules": { "curly": "error" } }',
      "nested/.eslintrc.json": '{ "rules": { "curly": "off" } }',
      "a.ts": readFileSync(fixture, "utf8"),
      "nested/b.ts": readFileSync(fixture, "utf8"),
    }),
  );

  expect(rules(findings, "nested/b.ts")).toContain("braces");
  expect(rules(findings, "a.ts")).not.toContain("braces");
  expect(rules(findings, "a.ts")).not.toHaveLength(0);
});

test("pre-commit remaps directories whose names start with two dots", () => {
  const findings = output(
    preCommit({
      ".eslintrc.json": '{ "rules": { "curly": "error" } }',
      "..sources/a.ts": readFileSync(fixture, "utf8"),
    }),
  );

  expect(rules(findings, "..sources/a.ts")).not.toContain("braces");
  expect(rules(findings, "..sources/a.ts")).not.toHaveLength(0);
});

test("pre-commit checks staged blobs instead of working tree files", () => {
  const cwd = repository();
  writeFileSync(join(cwd, "a.ts"), "export const clean = 1;\n");

  const result = Bun.spawnSync([join(rootWithoutBinary(), "git-hooks", "pre-commit")], {
    cwd,
    env: hookEnv(),
  });

  expect(rules(output(result), "a.ts")).toContain("braces");

  expect(result.exitCode).not.toBe(0);
});

test("the Stop hook forwards STANZA_FLAGS to stanza", () => {
  const original = readFileSync(fixture, "utf8");
  expect(braces(stopHook())).toBeLessThan(braces(original));
  expect(braces(stopHook("--no-braces"))).toBe(braces(original));
});
