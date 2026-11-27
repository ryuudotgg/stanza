import { expect, test } from "bun:test";
import { readFileSync, realpathSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import {
  git,
  gitFailing,
  gitWithoutAttrSource,
  rot13FilterProcess,
  run,
  scratchGitRepository,
} from "./support.ts";

const source = readFileSync(
  join(import.meta.dir, "fixtures", "braces", "bodies.before.ts"),
  "utf8",
);

function repository(files: Record<string, string> = { "a.ts": source }): string {
  const staged = Object.keys(files).filter((path) => !path.startsWith("node_modules/"));
  return scratchGitRepository({ files, staged });
}

function stagedCheck(cwd: string, ...flags: string[]): ReturnType<typeof run> {
  return run({ cwd }, "--check", "--staged", ...flags);
}

function rules(findings: string, path: string): string[] {
  return findings
    .split("\n")
    .filter((line) => line.startsWith(`${path}:`))
    .map((line) => line.split(": ")[1] ?? "");
}

test("--check --staged --no-braces keeps the blank line rules", () => {
  expect(stagedCheck(repository()).stdout).toContain(": braces: ");

  const findings = stagedCheck(repository(), "--no-braces").stdout;
  expect(findings).not.toContain(": braces: ");
  expect(findings).toContain(": after-multiline: ");
});

test("--check --staged --braces overrides a braces enforcing config", () => {
  const files = { ".oxlintrc.json": '{ "rules": { "curly": "error" } }', "a.ts": source };
  expect(stagedCheck(repository(files)).stdout).not.toContain(": braces: ");
  expect(stagedCheck(repository(files), "--braces").stdout).toContain(": braces: ");
});

test("--check --staged resolves shared ESLint packages from the repository", () => {
  const cwd = repository({
    ".eslintrc.json": '{ "extends": ["@acme/eslint-config"] }',
    "node_modules/@acme/eslint-config/package.json":
      '{"name":"@acme/eslint-config","main":"index.json"}',
    "node_modules/@acme/eslint-config/index.json": '{ "rules": { "curly": "off" } }',
    "a.ts": source,
  });

  expect(stagedCheck(cwd).stdout).toContain(": braces: ");
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

test("--check --staged names dirty and clean files apart from a quoted subdirectory", () => {
  const cwd = repository({ "it's/a.ts": source, "it's/b.ts": source, "c.ts": source });
  writeFileSync(join(cwd, "it's", "b.ts"), `${source}\n`);

  const result = run({ cwd: join(cwd, "it's") }, "--check", "--staged", "--no-braces");
  const inside = join(realpathSync(cwd), "it's");
  expect(result.stderr.trimEnd().split("\n")).toEqual([
    "3 findings in 3 staged files, all fixable",
    "these also have unstaged changes, fix them with --fix and restage by hand: b.ts",
    `fix and restage with: cd '${inside.replaceAll("'", "'\\''")}' && stanza --fix --no-braces -- ../c.ts a.ts && git --literal-pathspecs add -- ../c.ts a.ts`,
  ]);

  expect(result.code).toBe(1);
});

test("--check --staged reports names starting with a dash or holding a newline", () => {
  const cwd = repository({ "-x.ts": source, "a\nb.ts": source });
  const result = stagedCheck(cwd);

  expect(result.stdout).toMatch(/^-x\.ts:\d+:\d+: braces: /m);
  expect(result.stdout).toMatch(/^"a\\nb\.ts":\d+:\d+: braces: /m);
  expect(result.stderr.trimEnd()).toBe(
    `18 findings in 2 staged files, all fixable\nfix and restage with: cd ${realpathSync(cwd)} && stanza --fix -- -x.ts 'a\nb.ts' && git --literal-pathspecs add -- -x.ts 'a\nb.ts'`,
  );

  expect(result.code).toBe(1);
});

test("--check --staged reads filtered blobs through their smudge filter", () => {
  const cwd = scratchGitRepository();
  git(cwd, "config", "filter.rot.clean", "tr a-zA-Z n-za-mN-ZA-M");
  git(cwd, "config", "filter.rot.smudge", "tr a-zA-Z n-za-mN-ZA-M");

  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=rot\n");
  writeFileSync(join(cwd, "a.ts"), source);
  git(cwd, "add", ".");

  const findings = stagedCheck(cwd).stdout;
  expect(rules(findings, "a.ts")).toContain("braces");
  expect(rules(findings, "a.ts")).not.toContain("parse");

  const older = run({ cwd, env: gitWithoutAttrSource() }, "--check", "--staged");

  expect(older.stdout).toBe(findings);
});

test("--check --staged names the git that converts with differing index attributes", () => {
  const cwd = scratchGitRepository();
  git(cwd, "config", "filter.id.clean", "cat");
  git(cwd, "config", "filter.id.smudge", "cat");

  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=id\n");
  writeFileSync(join(cwd, "a.ts"), "if (a) {\n  b();\n}\n");
  git(cwd, "add", ".");

  const older = gitWithoutAttrSource();
  const failing = gitFailing("*cat-file*", "forced filter failure", 128);
  const finding = "a.ts:1:8: braces: remove the braces around this single statement body\n";
  const cases: [string, NodeJS.ProcessEnv, number, string, string | RegExp][] = [
    [
      "",
      older,
      2,
      "",
      "stanza: git cat-file failed: --attr-source is unsupported; staged files whose conversion attributes differ from the working tree need git 2.41 or later\n",
    ],
    ["", {}, 1, finding, /^1 finding in 1 staged file/],
    ["", failing, 2, "", "stanza: git cat-file failed: forced filter failure\n"],
    ["*.ts filter=id\n", older, 1, finding, /^1 finding in 1 staged file/],
  ];

  for (const [attributes, env, code, stdout, stderr] of cases) {
    writeFileSync(join(cwd, ".gitattributes"), attributes);
    const result = run({ cwd, env }, "--check", "--staged");

    expect(result.stdout).toBe(stdout);
    expect(result.code).toBe(code);

    if (typeof stderr === "string") expect(result.stderr).toBe(stderr);
    else expect(result.stderr).toMatch(stderr);
  }
});

test("--check --staged smudges every blob a filter process did not abort on", () => {
  const cwd = scratchGitRepository();
  git(cwd, "config", "filter.rot.process", rot13FilterProcess("abort.ts"));

  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=rot\n");
  writeFileSync(join(cwd, "abort.ts"), source);
  writeFileSync(join(cwd, "b.ts"), source);
  git(cwd, "add", ".");

  const findings = stagedCheck(cwd).stdout;
  expect(rules(findings, "b.ts")).toContain("braces");
  expect(rules(findings, "b.ts")).not.toContain("parse");
});

test("--check --staged does not restage a file with unstaged changes", () => {
  const cwd = repository();
  writeFileSync(join(cwd, "a.ts"), `${source}console.log("wip");\n`);

  expect(stagedCheck(cwd).stderr).toBe(
    "9 findings in 1 staged file, all fixable\nthese also have unstaged changes, fix them with --fix and restage by hand: a.ts\n",
  );
});

test("--check --staged smudges with the filter the index names", () => {
  const cwd = scratchGitRepository();
  git(cwd, "config", "filter.rot.clean", "tr a-zA-Z n-za-mN-ZA-M");
  git(cwd, "config", "filter.rot.smudge", "tr a-zA-Z n-za-mN-ZA-M");

  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=rot\n");
  writeFileSync(join(cwd, "a.ts"), source);
  git(cwd, "add", ".");
  writeFileSync(join(cwd, ".gitattributes"), "");

  const findings = stagedCheck(cwd).stdout;
  expect(rules(findings, "a.ts")).toContain("braces");
  expect(rules(findings, "a.ts")).not.toContain("parse");
});

test.each(["working-tree-encoding=UTF-16LE", "text", "eol=crlf", "crlf", "ident"])(
  "--check --staged converts a filtered blob with the index attributes, not a worktree %s",
  (value) => {
    const cwd = scratchGitRepository();
    git(cwd, "config", "filter.id.clean", "cat");
    git(cwd, "config", "filter.id.smudge", "cat");

    writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=id\n");
    writeFileSync(join(cwd, "a.ts"), "if (a) {\n  b();\n}\n");
    git(cwd, "add", ".");

    const check = () => {
      const result = stagedCheck(cwd, "--json");
      const rows = JSON.parse(result.stdout) as { rule: string; line: number; col: number }[];
      return { code: result.code, rows: rows.map(({ rule, line, col }) => ({ rule, line, col })) };
    };

    const expected = { code: 1, rows: [{ rule: "braces", line: 1, col: 8 }] };
    expect(check()).toEqual(expected);

    writeFileSync(join(cwd, ".gitattributes"), `*.ts filter=id ${value}\n`);
    expect(check()).toEqual(expected);
  },
);
