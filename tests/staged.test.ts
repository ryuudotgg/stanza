import { expect, test } from "bun:test";
import { chmodSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { scratch, scratchGitRepository, spawnCli } from "./support.ts";

const source = readFileSync(
  join(import.meta.dir, "fixtures", "braces", "bodies.before.ts"),
  "utf8",
);

function repository(files: Record<string, string> = { "a.ts": source }): string {
  const staged = Object.keys(files).filter((path) => !path.startsWith("node_modules/"));
  return scratchGitRepository({ files, staged });
}

function stagedCheck(cwd: string, ...flags: string[]): ReturnType<typeof spawnCli> {
  return spawnCli({ cwd }, "--check", "--staged", ...flags);
}

function rules(findings: string, path: string): string[] {
  return findings
    .split("\n")
    .filter((line) => line.startsWith(`${path}:`))
    .map((line) => line.split(" ")[1] ?? "");
}

test("--check --staged --no-braces keeps the blank line rules", () => {
  expect(stagedCheck(repository()).stdout).toContain(" braces ");

  const findings = stagedCheck(repository(), "--no-braces").stdout;
  expect(findings).not.toContain(" braces ");
  expect(findings).toContain(" after-multiline ");
});

test("--check --staged --braces overrides a braces enforcing config", () => {
  const files = { ".oxlintrc.json": '{ "rules": { "curly": "error" } }', "a.ts": source };
  expect(stagedCheck(repository(files)).stdout).not.toContain(" braces ");
  expect(stagedCheck(repository(files), "--braces").stdout).toContain(" braces ");
});

test("--check --staged resolves shared ESLint packages from the repository", () => {
  const cwd = repository({
    ".eslintrc.json": '{ "extends": ["@acme/eslint-config"] }',
    "node_modules/@acme/eslint-config/package.json":
      '{"name":"@acme/eslint-config","main":"index.json"}',
    "node_modules/@acme/eslint-config/index.json": '{ "rules": { "curly": "off" } }',
    "a.ts": source,
  });

  expect(stagedCheck(cwd).stdout).toContain(" braces ");
});

test("--check --staged resolves nested ESLint config from the repository", () => {
  const cwd = repository({
    ".eslintrc.json": '{ "rules": { "curly": "error" } }',
    "nested/.eslintrc.json": '{ "rules": { "curly": "off" } }',
    "a.ts": source,
    "nested/b.ts": source,
  });

  const findings = stagedCheck(cwd).stdout;
  expect(rules(findings, "nested/b.ts")).toContain("braces");
  expect(rules(findings, "a.ts")).not.toContain("braces");
  expect(rules(findings, "a.ts")).not.toHaveLength(0);
});

test("--check --staged reads directories whose names start with two dots", () => {
  const cwd = repository({
    ".eslintrc.json": '{ "rules": { "curly": "error" } }',
    "..sources/a.ts": source,
  });

  const findings = stagedCheck(cwd).stdout;
  expect(rules(findings, "..sources/a.ts")).not.toContain("braces");
  expect(rules(findings, "..sources/a.ts")).not.toHaveLength(0);
});

test("--check --staged checks staged blobs instead of working tree files", () => {
  const cwd = repository();
  writeFileSync(join(cwd, "a.ts"), "export const clean = 1;\n");

  const result = stagedCheck(cwd);
  expect(rules(result.stdout, "a.ts")).toContain("braces");
  expect(result.code).toBe(1);
});

test("--check --staged skips staged files marked linguist-generated", () => {
  const result = stagedCheck(
    repository({ ".gitattributes": "gen.ts linguist-generated\n", "gen.ts": source }),
  );

  expect(result.stdout).toBe("");
  expect(result.code).toBe(0);
});

test("--check --staged ends a failing run with the command that fixes and restages", () => {
  const result = stagedCheck(repository({ "a.ts": source, "b c.ts": "export {};\n" }));
  expect(result.stderr.trimEnd().split("\n").at(-1)).toEndWith(
    " && stanza --fix -- a.ts && git --literal-pathspecs add -- a.ts",
  );

  expect(result.code).toBe(1);
  expect(stagedCheck(repository(), "--braces").stderr).toContain(
    " && stanza --fix --braces -- a.ts && git --literal-pathspecs add -- a.ts",
  );
});

test("--check --staged reports names starting with a dash or holding a newline", () => {
  const cwd = repository({ "-x.ts": source, "a\nb.ts": source });
  const result = stagedCheck(cwd);

  expect(result.stdout).toMatch(/^-x\.ts:\d+:\d+ braces /m);
  expect(result.stdout).toMatch(/^a\nb\.ts:\d+:\d+ braces /m);
  expect(result.stderr.trimEnd()).toBe(
    `fix and restage with: cd ${realpathSync(cwd)} && stanza --fix -- -x.ts 'a\nb.ts' && git --literal-pathspecs add -- -x.ts 'a\nb.ts'`,
  );

  expect(result.code).toBe(1);
});

test("--check --staged reads filtered blobs through their smudge filter", () => {
  const cwd = scratchGitRepository();
  const git = (...args: string[]) => Bun.spawnSync(["git", ...args], { cwd });
  git("config", "filter.rot.clean", "tr a-zA-Z n-za-mN-ZA-M");
  git("config", "filter.rot.smudge", "tr a-zA-Z n-za-mN-ZA-M");

  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=rot\n");
  writeFileSync(join(cwd, "a.ts"), source);
  git("add", ".");

  const findings = stagedCheck(cwd).stdout;
  expect(rules(findings, "a.ts")).toContain("braces");
  expect(rules(findings, "a.ts")).not.toContain("parse");

  const shim = scratch("old-git");
  writeFileSync(
    join(shim, "git"),
    `#!/bin/sh\ncase "$*" in *--attr-source*) echo "unknown option" >&2; exit 129;; esac\nexec "${Bun.which("git")}" "$@"\n`,
  );

  chmodSync(join(shim, "git"), 0o755);

  const older = spawnCli(
    { cwd, env: { ...process.env, PATH: `${shim}:${process.env.PATH}` } },
    "--check",
    "--staged",
  );

  expect(older.stdout).toBe(findings);
});

test("--check --staged does not restage a file with unstaged changes", () => {
  const cwd = repository();
  writeFileSync(join(cwd, "a.ts"), `${source}console.log("wip");\n`);

  expect(stagedCheck(cwd).stderr).toBe(
    "these also have unstaged changes, fix them with --fix and restage by hand: a.ts\n",
  );
});

test("--check --staged smudges with the filter the index names", () => {
  const cwd = scratchGitRepository();
  const git = (...args: string[]) => Bun.spawnSync(["git", ...args], { cwd });
  git("config", "filter.rot.clean", "tr a-zA-Z n-za-mN-ZA-M");
  git("config", "filter.rot.smudge", "tr a-zA-Z n-za-mN-ZA-M");

  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=rot\n");
  writeFileSync(join(cwd, "a.ts"), source);
  git("add", ".");
  writeFileSync(join(cwd, ".gitattributes"), "");

  const findings = stagedCheck(cwd).stdout;
  expect(rules(findings, "a.ts")).toContain("braces");
  expect(rules(findings, "a.ts")).not.toContain("parse");
});

test("--check --staged reads attributes from the index", () => {
  const cwd = repository();
  writeFileSync(join(cwd, ".gitattributes"), "a.ts linguist-generated\n");
  expect(rules(stagedCheck(cwd).stdout, "a.ts")).toContain("braces");
});
