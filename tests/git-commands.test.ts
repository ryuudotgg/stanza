import { expect, test } from "bun:test";
import { mkdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { gitBinary, scratch, scratchGitRepository, spawnCli } from "./support.ts";

const wall = "f();\n".repeat(7);
const authored = {
  GIT_AUTHOR_NAME: "t",
  GIT_AUTHOR_EMAIL: "t@t",
  GIT_COMMITTER_NAME: "t",
  GIT_COMMITTER_EMAIL: "t@t",
};

function gitIn(cwd: string, ...args: string[]): void {
  const result = Bun.spawnSync([gitBinary, "-c", "commit.gpgsign=false", ...args], {
    cwd,
    env: { ...process.env, ...authored },
  });

  if (result.exitCode !== 0) throw new Error(new TextDecoder().decode(result.stderr));
}

function committed(files: Record<string, string>): string {
  const cwd = scratchGitRepository({ files, staged: true });
  gitIn(cwd, "commit", "-qm", "init");
  return cwd;
}

interface Call {
  args: string[];
  stdin?: string;
}

function call(command: string, stdin?: string): Call {
  const args = ["-C", "<root>", ...command.split(" ")];
  return stdin === undefined ? { args } : { args, stdin };
}

function blobAliases(cwd: string): Map<string, string> {
  const listed = Bun.spawnSync([gitBinary, "ls-files", "-s", "-z"], { cwd });
  const aliases = new Map<string, string>();
  for (const entry of new TextDecoder().decode(listed.stdout).split("\0").filter(Boolean)) {
    const [meta = "", path = ""] = entry.split("\t");
    aliases.set(meta.split(" ")[1]!, `<blob:${path}>`);
  }

  return aliases;
}

function commands(cwd: string, code: number, ...args: string[]): Call[] {
  const bin = scratch("git-shim");
  const log = join(bin, "calls.log");
  const real = Bun.which(gitBinary) ?? gitBinary;
  writeFileSync(log, "");
  writeFileSync(
    join(bin, "git"),
    `#!/bin/sh
{ for arg in "$@"; do printf '%s\\037' "$arg"; done; } >> '${log}'
case " $* " in
  *" --stdin "*|*" --batch "*)
    input=$(mktemp '${bin}/stdin.XXXXXX')
    cat > "$input"
    { printf '\\036'; tr '\\000\\n' '|;' < "$input"; printf '\\n'; } >> '${log}'
    exec '${real}' "$@" < "$input";;
esac
printf '\\n' >> '${log}'
exec '${real}' "$@"
`,
    { mode: 0o755 },
  );

  const aliases = blobAliases(cwd);
  const env = { ...process.env, PATH: `${bin}:${process.env.PATH}` };
  expect(spawnCli({ cwd, env }, ...args).code).toBe(code);

  const root = realpathSync(cwd);
  const shown = (text: string) =>
    text
      .replaceAll(root, "<root>")
      .replaceAll(/\b[0-9a-f]{40,64}\b/g, (oid) => aliases.get(oid) ?? "<oid>");

  const calls = readFileSync(log, "utf8")
    .split("\n")
    .filter((line) => line !== "")
    .map((line) => {
      const [argv = "", stdin] = shown(line).split("\x1e");
      const args = argv.split("\x1f").slice(0, -1);
      return stdin === undefined ? { args } : { args, stdin };
    });

  return calls;
}

const located = call("var -l");
const diffHead = call(
  "--literal-pathspecs diff --name-only -z --no-renames --submodule=short HEAD --",
);

const untracked = call("--literal-pathspecs ls-files -z --others --exclude-standard --");
const stagedDiff = call("diff --cached --diff-filter=ACMT --raw -z --no-renames --no-abbrev");
const unstaged = call("diff --name-only -z --no-renames");

test("--changed and --staged before the first commit spawn the reference's git commands", () => {
  const cwd = scratchGitRepository({
    files: { "a.ts": wall, "b.ts": wall },
    staged: ["a.ts"],
  });

  expect(commands(cwd, 1, "--check", "--changed")).toEqual([
    located,
    diffHead,
    call("rev-parse --verify -q HEAD"),
    call("symbolic-ref -q HEAD"),
    call("--literal-pathspecs ls-files -z --cached --"),
    untracked,
  ]);

  expect(commands(cwd, 1, "--check", "--staged")).toEqual([
    located,
    stagedDiff,
    call("check-attr --cached linguist-generated stanza filter -z --stdin", "a.ts|"),
    unstaged,
    call("cat-file --batch", "<blob:a.ts>;"),
  ]);

  gitIn(cwd, "symbolic-ref", "HEAD", "refs/heads/fresh");
  mkdirSync(join(cwd, ".git", "refs", "heads"), { recursive: true });
  writeFileSync(join(cwd, ".git", "refs", "heads", "fresh"), "junk\n");
  expect(commands(cwd, 2, "--check", "--changed")).toEqual([
    located,
    diffHead,
    call("rev-parse --verify -q HEAD"),
    call("symbolic-ref -q HEAD"),
  ]);
});

test("--changed and --staged with untracked files spawn the reference's git commands", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  writeFileSync(join(cwd, "a.ts"), wall);
  writeFileSync(join(cwd, "c.ts"), wall);
  gitIn(cwd, "add", "a.ts");
  writeFileSync(join(cwd, "a.ts"), `${wall}g();\n`);

  expect(commands(cwd, 1, "--check", "--changed")).toEqual([located, diffHead, untracked]);
  expect(commands(cwd, 1, "--check", "--staged")).toEqual([
    located,
    stagedDiff,
    call("check-attr --cached linguist-generated stanza filter -z --stdin", "a.ts|"),
    unstaged,
    call("cat-file --batch", "<blob:a.ts>;"),
  ]);
});

test("a clean filter routes --staged blobs through cat-file --filters", () => {
  const cwd = scratchGitRepository();
  gitIn(cwd, "config", "filter.rot.clean", "tr a-zA-Z n-za-mN-ZA-M");
  gitIn(cwd, "config", "filter.rot.smudge", "tr a-zA-Z n-za-mN-ZA-M");

  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=rot\n");
  for (const name of ["b.ts", "a.ts", "plain.js"]) writeFileSync(join(cwd, name), wall);
  gitIn(cwd, "add", ".");
  gitIn(cwd, "commit", "-qm", "init");

  for (const name of ["b.ts", "a.ts", "plain.js"])
    writeFileSync(join(cwd, name), `${wall}${name.at(0)}();\n`);

  gitIn(cwd, "add", ".");

  expect(commands(cwd, 1, "--check", "--changed")).toEqual([
    located,
    diffHead,
    untracked,
    call("check-attr linguist-generated stanza -z --stdin", "a.ts|b.ts|plain.js|"),
  ]);

  const filtered = [
    located,
    stagedDiff,
    call("check-attr --cached linguist-generated stanza filter -z --stdin", "a.ts|b.ts|plain.js|"),
    unstaged,
    call("check-attr stanza -z --stdin", "a.ts|b.ts|plain.js|"),
    call("cat-file --batch", "<blob:plain.js>;"),
    call("check-attr filter -z --stdin", "a.ts|b.ts|"),
  ];

  expect(commands(cwd, 1, "--check", "--staged")).toEqual([
    ...filtered,
    call("cat-file --filters --path=a.ts <blob:a.ts>"),
    call("cat-file --filters --path=b.ts <blob:b.ts>"),
  ]);

  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=other\n");
  expect(commands(cwd, 1, "--check", "--staged")).toEqual([
    ...filtered,
    call("write-tree"),
    call("--attr-source=<oid> cat-file --filters --path=a.ts <blob:a.ts>"),
    call("--attr-source=<oid> cat-file --filters --path=b.ts <blob:b.ts>"),
  ]);
});

test("a changed or staged submodule spawns the reference's git commands", () => {
  const sub = committed({ "s.ts": wall });
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  gitIn(cwd, "-c", "protocol.file.allow=always", "submodule", "add", "-q", sub, "vendor/sub");
  gitIn(cwd, "commit", "-qm", "sub");

  writeFileSync(join(cwd, "vendor", "sub", "s.ts"), "g();\n");
  gitIn(join(cwd, "vendor", "sub"), "commit", "-qam", "next");

  expect(commands(cwd, 0, "--check", "--changed")).toEqual([
    located,
    diffHead,
    untracked,
    call("config -z --file .gitmodules --get-regexp ^submodule\\..*\\.path$"),
  ]);

  gitIn(cwd, "add", "vendor/sub");
  expect(commands(cwd, 0, "--check", "--staged")).toEqual([located, stagedDiff, unstaged]);
});
