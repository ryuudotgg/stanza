import { afterAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  gitBinary,
  rot13FilterProcess,
  scratch,
  scratchGitRepository,
  spawnCli,
} from "./support.ts";

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
  return traced(cwd, code, args);
}

const shim = realpathSync(mkdtempSync(join(tmpdir(), "stanza-git-shim-")));
afterAll(() => rmSync(shim, { recursive: true, force: true }));
writeFileSync(
  join(shim, "git"),
  `#!/bin/sh
line=$({ for arg in "$@"; do printf '%s\\037' "$arg"; done | tr '\\n' ';'; }; printf x)
line=\${line%x}
case " $* " in
  *" --stdin "*|*" --batch "*)
    input=$(mktemp "$SHIM_LOG.XXXXXX")
    cat > "$input"
    printf '%s\\036%s\\n' "$line" "$(tr '\\000\\n' '|;' < "$input")" >> "$SHIM_LOG"
    exec '${Bun.which(gitBinary) ?? gitBinary}' "$@" < "$input";;
esac
printf '%s\\n' "$line" >> "$SHIM_LOG"
if [ -n "$SHIM_MEET" ] && [ "$3 $4" = "var -l" ]; then
  touch "$SHIM_MEET/$$"
  waited=0
  while [ "$(ls "$SHIM_MEET" | wc -l)" -lt 2 ] && [ "$waited" -lt 40 ]; do
    sleep 0.05
    waited=$((waited + 1))
  done
  ls "$SHIM_MEET" | wc -l | tr -d ' ' >> "$SHIM_MEET.met"
fi
exec '${Bun.which(gitBinary) ?? gitBinary}' "$@"
`,
  { mode: 0o755 },
);

function traced(cwd: string, code: number, args: string[], stdin?: string, meet?: string): Call[] {
  const log = join(scratch("git-calls"), "calls.log");
  writeFileSync(log, "");

  const aliases = blobAliases(cwd);
  const env = {
    ...process.env,
    AGENT_HOOKS: "1",
    SHIM_LOG: log,
    SHIM_MEET: meet ?? "",
    PATH: `${shim}:${process.env.PATH}`,
  };

  const input = stdin === undefined ? undefined : new TextEncoder().encode(stdin);
  expect(spawnCli({ cwd, env, stdin: input }, ...args).code).toBe(code);

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
const changedWave = [located, diffHead, untracked];
const stagedDiff = call("diff --cached --diff-filter=ACMT --raw -z --no-renames --no-abbrev");
const unstaged = call("diff --name-only -z --no-renames");

function expectWaves(calls: Call[], ...waves: Call[][]): void {
  const asSet = (wave: Call[]) => wave.map((entry) => JSON.stringify(entry)).toSorted();
  let start = 0;
  const actual = waves.map((wave) => asSet(calls.slice(start, (start += wave.length))));

  expect(calls.length).toBe(start);
  expect(actual).toEqual(waves.map(asSet));
}

test("--changed and --staged before the first commit spawn the reference's git commands", () => {
  const cwd = scratchGitRepository({
    files: { "a.ts": wall, "b.ts": wall },
    staged: ["a.ts"],
  });

  expectWaves(
    commands(cwd, 1, "--check", "--changed"),
    changedWave,
    [call("rev-parse --verify -q HEAD")],
    [call("symbolic-ref -q HEAD")],
    [call("--literal-pathspecs ls-files -z --cached --")],
  );

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
  expectWaves(
    commands(cwd, 2, "--check", "--changed"),
    changedWave,
    [call("rev-parse --verify -q HEAD")],
    [call("symbolic-ref -q HEAD")],
  );
});

test("--changed and --staged with untracked files spawn the reference's git commands", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  writeFileSync(join(cwd, "a.ts"), wall);
  writeFileSync(join(cwd, "c.ts"), wall);
  gitIn(cwd, "add", "a.ts");
  writeFileSync(join(cwd, "a.ts"), `${wall}g();\n`);

  expectWaves(commands(cwd, 1, "--check", "--changed"), changedWave);
  expect(commands(cwd, 1, "--check", "--staged")).toEqual([
    located,
    stagedDiff,
    call("check-attr --cached linguist-generated stanza filter -z --stdin", "a.ts|"),
    unstaged,
    call("cat-file --batch", "<blob:a.ts>;"),
  ]);
});

test("--hunks adds the reference's hunk diff to --changed and --staged", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  writeFileSync(join(cwd, "a.ts"), wall);
  writeFileSync(join(cwd, "c.ts"), wall);
  gitIn(cwd, "add", "a.ts");
  writeFileSync(join(cwd, "a.ts"), `${wall}g();\n`);

  const hunkDiff = (range: string) =>
    call(
      `-c diff.suppressBlankEmpty=false --literal-pathspecs diff --unified=999999999 --text --submodule=short --no-color --no-ext-diff --no-textconv --no-renames ${range} -- a.ts`,
    );

  expectWaves(commands(cwd, 1, "--check", "--changed", "--hunks"), changedWave, [hunkDiff("HEAD")]);

  expect(commands(cwd, 1, "--check", "--staged", "--hunks")).toEqual([
    located,
    stagedDiff,
    hunkDiff("--cached --diff-filter=ACMT"),
    call("check-attr --cached linguist-generated stanza filter -z --stdin", "a.ts|"),
    unstaged,
    call("cat-file --batch", "<blob:a.ts>;"),
  ]);
});

test("a clean filter routes --staged blobs through one cat-file --batch --filters", () => {
  const cwd = scratchGitRepository();
  gitIn(cwd, "config", "filter.rot.clean", "tr a-zA-Z n-za-mN-ZA-M");
  gitIn(cwd, "config", "filter.rot.smudge", "tr a-zA-Z n-za-mN-ZA-M");

  const names = ["e.ts", "b.ts", "a.ts", "d.ts", "c.ts", "n\nl.ts", "plain.js"];
  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=rot\n");
  for (const name of names) writeFileSync(join(cwd, name), wall);
  gitIn(cwd, "add", ".");
  gitIn(cwd, "commit", "-qm", "init");

  names.forEach((name, index) => writeFileSync(join(cwd, name), `${wall}f${index}();\n`));
  gitIn(cwd, "add", ".");

  expectWaves(commands(cwd, 1, "--check", "--changed"), changedWave);

  const smudged = ["a.ts", "b.ts", "c.ts", "d.ts", "e.ts"];
  const filtered = [
    located,
    stagedDiff,
    call(
      "check-attr --cached linguist-generated stanza filter -z --stdin",
      `${smudged.join("|")}|n;l.ts|plain.js|`,
    ),
    unstaged,
    call("cat-file --batch", "<blob:plain.js>;"),
    call("check-attr filter -z --stdin", `${smudged.join("|")}|n;l.ts|`),
  ];

  const batch = smudged.map((name) => `<blob:${name}> ${name};`).join("");
  expect(commands(cwd, 1, "--check", "--staged")).toEqual([
    ...filtered,
    call("cat-file --batch --filters", batch),
    call("cat-file --filters --path=n;l.ts <blob:n\nl.ts>"),
  ]);

  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=other\n");
  expect(commands(cwd, 1, "--check", "--staged")).toEqual([
    ...filtered,
    call("write-tree"),
    call("--attr-source=<oid> cat-file --batch --filters", batch),
    call("--attr-source=<oid> cat-file --filters --path=n;l.ts <blob:n\nl.ts>"),
  ]);

  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=rot\n");
  gitIn(cwd, "config", "filter.rot.process", rot13FilterProcess());
  expect(commands(cwd, 1, "--check", "--staged")).toEqual([
    ...filtered,
    ...[...smudged, "n\nl.ts"].map((name) =>
      call(`cat-file --filters --path=${name.replace("\n", ";")} <blob:${name}>`),
    ),
  ]);
});

test("a changed or staged submodule spawns the reference's git commands", () => {
  const sub = committed({ "s.ts": wall });
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  gitIn(cwd, "-c", "protocol.file.allow=always", "submodule", "add", "-q", sub, "vendor/sub");
  gitIn(cwd, "commit", "-qm", "sub");

  writeFileSync(join(cwd, "vendor", "sub", "s.ts"), "g();\n");
  gitIn(join(cwd, "vendor", "sub"), "commit", "-qam", "next");

  expectWaves(commands(cwd, 0, "--check", "--changed"), changedWave, [
    call("config -z --file .gitmodules --get-regexp ^submodule\\..*\\.path$"),
  ]);

  gitIn(cwd, "add", "vendor/sub");
  expect(commands(cwd, 0, "--check", "--staged")).toEqual([located, stagedDiff, unstaged]);
});

test("a predicted root that var -l reports as configured elsewhere takes the located path", () => {
  const cwd = committed({ "sub/a.ts": "export const a = 1;\n" });
  writeFileSync(join(cwd, "sub", "a.ts"), wall);
  gitIn(cwd, "config", "core.worktree", join(realpathSync(cwd), "sub"));

  const inSub = ({ args, stdin }: Call) => ({
    args: ["-C", "<root>/sub", ...args.slice(2)],
    stdin,
  });

  expectWaves(
    commands(cwd, 1, "--check", "--changed"),
    changedWave,
    [call("rev-parse --show-toplevel")],
    [inSub(diffHead)],
    [inSub(untracked)],
    [inSub(call("check-attr linguist-generated stanza -z --stdin", "a.ts|"))],
  );

  expect(spawnCli({ cwd }, "--check", "--changed").stdout).toContain("a.ts");
});

test("a predicted root git refuses reports git's discovery error", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  writeFileSync(join(cwd, "a.ts"), wall);
  writeFileSync(
    join(cwd, ".git", "config"),
    `${readFileSync(join(cwd, ".git", "config"), "utf8")}not a config line\n`,
  );

  expectWaves(commands(cwd, 2, "--check", "--changed"), changedWave, [
    call("rev-parse --show-toplevel"),
  ]);

  const { stderr } = spawnCli({ cwd }, "--check", "--changed");
  expect(stderr).toContain("git rev-parse failed");
  expect(stderr).not.toContain("git diff failed");
});

test("a repository with extensions reuses the wave once git names the same root", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  writeFileSync(join(cwd, "a.ts"), wall);
  gitIn(cwd, "config", "core.repositoryformatversion", "1");
  gitIn(cwd, "config", "extensions.worktreeConfig", "true");

  expectWaves(commands(cwd, 1, "--check", "--changed"), changedWave, [
    call("rev-parse --show-toplevel"),
  ]);

  expect(spawnCli({ cwd }, "--check", "--changed").stdout).toContain("a.ts");
});

test("a Stop over two repositories starts both repositories' calls as one wave", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  const inner = join(cwd, "inner");
  mkdirSync(inner);
  gitIn(inner, "init", "-q");

  writeFileSync(join(inner, "b.ts"), "export const b = 1;\n");
  gitIn(inner, "add", "b.ts");
  gitIn(inner, "commit", "-qm", "init");

  const written = [join(cwd, "a.ts"), join(inner, "b.ts")];
  for (const file of written) writeFileSync(file, "export const value = 2;\n");

  const transcript = join(scratch("transcript"), "session.jsonl");
  const content = written.map((file_path, index) => ({
    type: "tool_use",
    id: `toolu_${index}`,
    name: "Write",
    input: { file_path, content: "export const value = 2;\n" },
  }));

  writeFileSync(transcript, `${JSON.stringify({ type: "assistant", message: { content } })}\n`);
  const stop = JSON.stringify({ cwd, hook_event_name: "Stop", transcript_path: transcript });
  const scoped = (root: string, { args }: Call, path: string) => ({
    args: ["-C", root, ...args.slice(2), path],
  });

  const meet = join(scratch("meet"), "room");
  mkdirSync(meet);

  expectWaves(traced(cwd, 0, ["hook"], stop, meet), [
    located,
    scoped("<root>", diffHead, "a.ts"),
    { args: ["-C", "<root>/inner", "var", "-l"] },
    scoped("<root>/inner", diffHead, "b.ts"),
  ]);

  expect(readFileSync(`${meet}.met`, "utf8").split("\n").filter(Boolean)).toEqual(["2", "2"]);
});

test("a hook lists untracked files only when the index misses a path or cannot be read", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  writeFileSync(join(cwd, "a.ts"), "export const a = 2;\n");
  const edited = (name: string) =>
    JSON.stringify({
      cwd,
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      tool_input: { file_path: join(cwd, name) },
    });

  const scoped = ({ args }: Call, path: string) => ({ args: [...args, path] });

  expectWaves(traced(cwd, 0, ["hook"], edited("a.ts")), [located, scoped(diffHead, "a.ts")]);

  writeFileSync(join(cwd, "b.ts"), "export const b = 1;\n");
  expectWaves(traced(cwd, 0, ["hook"], edited("b.ts")), [
    located,
    scoped(diffHead, "b.ts"),
    scoped(untracked, "b.ts"),
  ]);

  gitIn(cwd, "update-index", "--index-version", "4");
  expectWaves(
    traced(cwd, 0, ["hook"], edited("a.ts")),
    [located, scoped(diffHead, "a.ts"), scoped(untracked, "a.ts")],
    [call("check-attr linguist-generated stanza -z --stdin", "a.ts|")],
  );

  gitIn(cwd, "update-index", "--index-version", "2");
  const tree = Bun.spawnSync([gitBinary, "rev-parse", "HEAD^{tree}"], { cwd }).stdout.toString();
  rmSync(join(cwd, ".git", "objects", tree.slice(0, 2), tree.slice(2).trim()));

  expectWaves(
    traced(cwd, 1, ["hook"], edited("a.ts")),
    [located, scoped(diffHead, "a.ts")],
    [call("rev-parse --verify -q HEAD")],
    [scoped(untracked, "a.ts")],
  );
});
