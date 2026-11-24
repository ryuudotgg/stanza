import { afterAll, expect, test } from "bun:test";
import {
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { countSpawned, cpuOf } from "./budget.ts";
import {
  colorless,
  git,
  gitBinary,
  gitIn,
  rot13FilterProcess,
  run,
  scratch,
  scratchGitRepository,
  stanzaBinary,
} from "./support.ts";

const wall = "f();\n".repeat(7);
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
if [ -n "$SHIM_MEET" ] && { [ "$3 $4" = "var -l" ] || [ "$3" = config ]; }; then
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
  expect(run({ cwd, env, stdin: input }, ...args).code).toBe(code);

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
const cached = call("--literal-pathspecs ls-files -z --cached --");
const indexedOrUntracked = call(
  "--literal-pathspecs ls-files -z --cached --others --exclude-standard --",
);

const listed = call("ls-files -z --cached --others --exclude-standard -- .");
const stagedDiff = call("diff --cached --diff-filter=ACMT --raw -z --no-renames --no-abbrev");
const unstaged = call("diff --name-only -z --no-renames");

function expectWaves(calls: Call[], ...waves: Call[][]): void {
  const asSet = (wave: Call[]) => wave.map((entry) => JSON.stringify(entry)).toSorted();
  let start = 0;
  const actual = waves.map((wave) => asSet(calls.slice(start, (start += wave.length))));

  expect(calls.length).toBe(start);
  expect(actual).toEqual(waves.map(asSet));
}

test("--changed and --staged before the first commit spawn exactly these git commands", () => {
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
    call(
      "check-attr --cached linguist-generated stanza filter working-tree-encoding text eol crlf ident -z --stdin",
      "a.ts|",
    ),
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

test("files below a skipped folder reach check-attr only when an attribute file can opt them in", () => {
  const files: Record<string, string> = { "a.ts": wall };
  for (let index = 0; index < 1000; index++) files[`dist/f${index}.ts`] = wall;
  const cwd = committed(files);

  expect(commands(cwd, 1, "--check", ".")).toEqual([located, listed]);
  expect(commands(cwd, 1, "--check", "a.ts", "dist/f0.ts")).toEqual([located]);

  writeFileSync(join(cwd, "dist", "f0.ts"), `${wall}g();\n`);
  gitIn(cwd, "add", "dist/f0.ts");
  expectWaves(commands(cwd, 0, "--check", "--changed"), changedWave);
  expect(commands(cwd, 0, "--check", "--staged")).toEqual([located, stagedDiff, unstaged]);

  writeFileSync(join(cwd, ".gitattributes"), "dist/f0.ts stanza\n");
  const attributes = "check-attr linguist-generated stanza -z --stdin";
  const opted = commands(cwd, 1, "--check", "a.ts", "dist/f0.ts");
  expect(opted).toEqual([located, call(attributes, "a.ts|dist/f0.ts|")]);
});

test("--changed and --staged with untracked files spawn exactly these git commands", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  writeFileSync(join(cwd, "a.ts"), wall);
  writeFileSync(join(cwd, "c.ts"), wall);
  gitIn(cwd, "add", "a.ts");
  writeFileSync(join(cwd, "a.ts"), `${wall}g();\n`);

  expectWaves(commands(cwd, 1, "--check", "--changed"), changedWave);
  expect(commands(cwd, 1, "--check", "--staged")).toEqual([
    located,
    stagedDiff,
    call(
      "check-attr --cached linguist-generated stanza filter working-tree-encoding text eol crlf ident -z --stdin",
      "a.ts|",
    ),
    unstaged,
    call("cat-file --batch", "<blob:a.ts>;"),
  ]);
});

test("--hunks adds one hunk diff to --changed and --staged", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  writeFileSync(join(cwd, "a.ts"), wall);
  writeFileSync(join(cwd, "c.ts"), wall);
  gitIn(cwd, "add", "a.ts");
  writeFileSync(join(cwd, "a.ts"), `${wall}g();\n`);

  const hunkDiff = (range: string) =>
    call(
      `-c diff.suppressBlankEmpty=false --literal-pathspecs diff --unified=999999999 --text --submodule=short --no-color --no-ext-diff --no-textconv --no-renames ${range} -- a.ts`,
    );

  expectWaves(
    commands(cwd, 1, "--check", "--changed", "--hunks"),
    changedWave,
    [call("check-attr linguist-generated stanza filter -z --stdin", "a.ts|c.ts|")],
    [hunkDiff("HEAD")],
  );

  expect(commands(cwd, 1, "--check", "--staged", "--hunks")).toEqual([
    located,
    stagedDiff,
    call(
      "check-attr --cached linguist-generated stanza filter working-tree-encoding text eol crlf ident -z --stdin",
      "a.ts|",
    ),
    hunkDiff("--cached --diff-filter=ACMT"),
    unstaged,
    call("cat-file --batch", "<blob:a.ts>;"),
  ]);
});

test("--hunks leaves a generated file out of the hunk diff under --changed and --staged", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n", "b.ts": "export const b = 1;\n" });
  writeFileSync(join(cwd, ".git", "info", "attributes"), "a.ts linguist-generated\n");
  writeFileSync(join(cwd, "a.ts"), wall);
  writeFileSync(join(cwd, "b.ts"), wall);

  const hunkDiff = (range: string) =>
    call(
      `-c diff.suppressBlankEmpty=false --literal-pathspecs diff --unified=999999999 --text --submodule=short --no-color --no-ext-diff --no-textconv --no-renames ${range} -- b.ts`,
    );

  expectWaves(
    commands(cwd, 1, "--check", "--changed", "--hunks"),
    changedWave,
    [call("check-attr linguist-generated stanza filter -z --stdin", "a.ts|b.ts|")],
    [hunkDiff("HEAD")],
  );

  gitIn(cwd, "add", "a.ts", "b.ts");
  expect(commands(cwd, 1, "--check", "--staged", "--hunks")).toEqual([
    located,
    stagedDiff,
    call(
      "check-attr --cached linguist-generated stanza filter working-tree-encoding text eol crlf ident -z --stdin",
      "a.ts|b.ts|",
    ),
    hunkDiff("--cached --diff-filter=ACMT"),
    unstaged,
    call("cat-file --batch", "<blob:b.ts>;"),
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
      "check-attr --cached linguist-generated stanza filter working-tree-encoding text eol crlf ident -z --stdin",
      `${smudged.join("|")}|n;l.ts|plain.js|`,
    ),
    unstaged,
    call("cat-file --batch", "<blob:plain.js>;"),
    call(
      "check-attr filter working-tree-encoding text eol crlf ident -z --stdin",
      `${smudged.join("|")}|n;l.ts|`,
    ),
  ];

  const batch = smudged.map((name) => `<blob:${name}> ${name};<oid>;`).join("");
  expect(commands(cwd, 1, "--check", "--staged")).toEqual([
    ...filtered,
    call("cat-file --batch --filters", batch),
    call("cat-file --filters --path=n;l.ts <blob:n\nl.ts>"),
  ]);

  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=other\n");
  expect(commands(cwd, 1, "--check", "--staged")).toEqual([
    ...filtered,
    call("-c core.splitIndex=false write-tree"),
    call("--attr-source=<oid> cat-file --batch --filters", batch),
    call("--attr-source=<oid> cat-file --filters --path=n;l.ts <blob:n\nl.ts>"),
  ]);

  for (const value of ["working-tree-encoding=UTF-16LE", "text", "eol=crlf", "crlf", "ident"]) {
    writeFileSync(join(cwd, ".gitattributes"), `*.ts filter=rot ${value}\n`);
    expect(commands(cwd, 1, "--check", "--staged")).toEqual([
      ...filtered,
      call("-c core.splitIndex=false write-tree"),
      call("--attr-source=<oid> cat-file --batch --filters", batch),
      call("--attr-source=<oid> cat-file --filters --path=n;l.ts <blob:n\nl.ts>"),
    ]);
  }

  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=rot\n");
  gitIn(cwd, "config", "filter.rot.process", rot13FilterProcess());
  expect(commands(cwd, 1, "--check", "--staged")).toEqual([
    ...filtered,
    ...[...smudged, "n\nl.ts"].map((name) =>
      call(`cat-file --filters --path=${name.replace("\n", ";")} <blob:${name}>`),
    ),
  ]);
}, 60_000);

const submoduleKeys = call("config -z --file .gitmodules --list");

function withSubmodule(files: Record<string, string>): string {
  const sub = committed({ "s.ts": wall });
  const cwd = scratchGitRepository({ files, staged: true });
  gitIn(cwd, "-c", "protocol.file.allow=always", "submodule", "add", "-q", sub, "vendor/sub");
  gitIn(cwd, "commit", "-qm", "sub");
  return cwd;
}

test("a changed or staged submodule spawns exactly these git commands", () => {
  const cwd = withSubmodule({ "a.ts": "export const a = 1;\n" });
  expect(commands(cwd, 0, "--check", ".")).toEqual([located, listed, submoduleKeys]);

  writeFileSync(join(cwd, "vendor", "sub", "s.ts"), "g();\n");
  gitIn(join(cwd, "vendor", "sub"), "commit", "-qam", "next");

  const meet = join(scratch("meet"), "room");
  mkdirSync(meet);
  expectWaves(traced(cwd, 0, ["--check", "--changed"], undefined, meet), [
    ...changedWave,
    submoduleKeys,
  ]);

  expect(readFileSync(`${meet}.met`, "utf8").split("\n").filter(Boolean)).toEqual(["2", "2"]);

  gitIn(cwd, "add", "vendor/sub");
  expect(commands(cwd, 0, "--check", "--staged")).toEqual([located, stagedDiff, unstaged]);
});

test("a run asks git for submodule paths once, and a hook never does", () => {
  const cwd = withSubmodule({ "src/a.ts": "export const a = 1;\n", "lib/b.ts": wall });
  const listedIn = (dir: string) => ({ args: ["-C", `<root>/${dir}`, ...listed.args.slice(2)] });
  expect(commands(cwd, 1, "--check", "src", "lib")).toEqual([
    located,
    listedIn("src"),
    submoduleKeys,
    listedIn("lib"),
  ]);

  const only = withSubmodule({});
  expect(commands(only, 0, "--check", ".")).toEqual([located, listed, submoduleKeys]);

  writeFileSync(join(cwd, "src", "a.ts"), "export const a = 2;\n");
  const edited = JSON.stringify({
    cwd,
    hook_event_name: "PostToolUse",
    tool_name: "Edit",
    tool_input: { file_path: join(cwd, "src", "a.ts") },
  });

  expectWaves(traced(cwd, 0, ["hook"], edited), [located, { args: [...cached.args, "src/a.ts"] }]);

  const stop = JSON.stringify({ cwd, hook_event_name: "Stop" });
  expectWaves(traced(cwd, 0, ["hook"], stop), changedWave);
});

test("a directory with no candidate lists its files once to explain the empty selection", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n", "docs/README.md": "hi\n" });
  expect(commands(cwd, 0, "--check", "docs")).toEqual([
    located,
    { args: ["-C", "<root>/docs", ...listed.args.slice(2)] },
  ]);
});

test("an empty selection takes its reason from the one attribute query", () => {
  const cwd = scratchGitRepository({
    files: { "a.ts": wall, ".gitattributes": "a.ts linguist-generated stanza\n" },
  });

  expect(commands(cwd, 0, "--check", ".")).toEqual([
    located,
    listed,
    call("check-attr linguist-generated stanza -z --stdin", "a.ts|"),
  ]);
});

test("a walk outside git spawns for a repository it meets what naming it spawns", () => {
  const cwd = scratch("nested-repository");
  mkdirSync(join(cwd, "child"));
  gitIn(join(cwd, "child"), "init", "-q");

  writeFileSync(join(cwd, "child", ".gitattributes"), "gen.ts linguist-generated\n");
  writeFileSync(join(cwd, "child", "gen.ts"), wall);
  writeFileSync(join(cwd, "child", "plain.ts"), wall);

  const named = commands(cwd, 1, "--check", "child");
  expect(named).toEqual([
    { args: ["-C", "<root>/child", ...located.args.slice(2)] },
    { args: ["-C", "<root>/child", ...listed.args.slice(2)] },
    {
      args: ["-C", "<root>/child", "check-attr", "linguist-generated", "stanza", "-z", "--stdin"],
      stdin: "gen.ts|plain.ts|",
    },
  ]);

  expect(commands(cwd, 1, "--check", ".")).toEqual(named);

  const plain = scratch("plain-folder");
  mkdirSync(join(plain, "inner"));
  writeFileSync(join(plain, "inner", "a.ts"), wall);
  expect(commands(plain, 1, "--check", ".")).toEqual([]);
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

  expect(run({ cwd }, "--check", "--changed").stdout).toContain("a.ts");
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

  const { stderr } = run({ cwd }, "--check", "--changed");
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

  expect(run({ cwd }, "--check", "--changed").stdout).toContain("a.ts");
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
    scoped("<root>", cached, "a.ts"),
    { args: ["-C", "<root>/inner", "var", "-l"] },
    scoped("<root>/inner", cached, "b.ts"),
  ]);

  expect(readFileSync(`${meet}.met`, "utf8").split("\n").filter(Boolean)).toEqual(["2", "2"]);
});

test("--check --changed and PostToolUse leave a stat dirty index as it was", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  const later = new Date(Date.now() + 5000);
  utimesSync(join(cwd, "a.ts"), later, later);

  const index = join(cwd, ".git", "index");
  const state = () => ({
    bytes: readFileSync(index).toString("hex"),
    mtime: statSync(index, { bigint: true }).mtimeNs,
  });

  const before = state();
  const edited = JSON.stringify({
    cwd,
    hook_event_name: "PostToolUse",
    tool_name: "Edit",
    tool_input: { file_path: join(cwd, "a.ts") },
  });

  const env = { ...process.env, AGENT_HOOKS: "1" };
  expect(run({ cwd }, "--check", "--changed").code).toBe(0);
  expect(run({ cwd, env, stdin: new TextEncoder().encode(edited) }, "hook").code).toBe(0);
  expect(state()).toEqual(before);

  gitIn(cwd, "diff", "--name-only", "HEAD");
  expect(state()).not.toEqual(before);
});

test("--check --changed deletes its private index before the first finding", async () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  writeFileSync(join(cwd, "a.ts"), "if (a) {\n  b();\n}\n".repeat(20_000));

  const tmp = scratch("tmp");
  const child = Bun.spawn([stanzaBinary(), "--check", "--changed"], {
    cwd,
    env: { ...colorless(), TMPDIR: tmp },
    stdout: "pipe",
    stderr: "ignore",
  });

  const first = await child.stdout.getReader().read();
  const left = readdirSync(tmp);

  child.kill();
  await child.exited;
  countSpawned(cpuOf({ resourceUsage: child.resourceUsage() }));

  expect(new TextDecoder().decode(first.value)).toStartWith("a.ts:1:8: braces");
  expect(left).toEqual([]);
});

test("--check --staged leaves a stat dirty index as it was, write-tree included", () => {
  const indexState = (cwd: string) => {
    const index = join(cwd, ".git", "index");
    return {
      bytes: readFileSync(index).toString("hex"),
      mtime: statSync(index, { bigint: true }).mtimeNs,
    };
  };

  const stageBraces = (cwd: string) => {
    writeFileSync(join(cwd, "a.ts"), "if (a) {\n  b();\n}\n");
    gitIn(cwd, "add", "a.ts");
    const later = new Date(Date.now() + 5000);
    utimesSync(join(cwd, "a.ts"), later, later);
  };

  const plain = committed({ "a.ts": "export const a = 1;\n" });
  stageBraces(plain);
  const plainBefore = indexState(plain);

  const checked = run({ cwd: plain }, "--check", "--staged");
  expect(checked.code).toBe(1);
  expect(checked.stdout).toContain("a.ts:1:8");
  expect(indexState(plain)).toEqual(plainBefore);

  const filtered = committed({ ".gitattributes": "*.ts filter=id\n", "a.ts": "export {};\n" });
  gitIn(filtered, "config", "filter.id.clean", "cat");
  gitIn(filtered, "config", "filter.id.smudge", "cat");
  stageBraces(filtered);
  writeFileSync(join(filtered, ".gitattributes"), "");

  const filteredBefore = indexState(filtered);
  expect(commands(filtered, 1, "--check", "--staged")).toContainEqual(
    call("-c core.splitIndex=false write-tree"),
  );

  expect(indexState(filtered)).toEqual(filteredBefore);

  gitIn(filtered, "write-tree");
  expect(indexState(filtered)).not.toEqual(filteredBefore);

  gitIn(filtered, "update-index", "--split-index");
  writeFileSync(join(filtered, "a.ts"), "if (c) {\n  d();\n}\n");
  gitIn(filtered, "add", "a.ts");
  gitIn(filtered, "config", "splitIndex.maxPercentChange", "0");
  gitIn(filtered, "config", "splitIndex.sharedIndexExpire", "now");

  const splitBefore = indexState(filtered);
  expect(run({ cwd: filtered }, "--check", "--staged").code).toBe(1);
  expect(indexState(filtered)).toEqual(splitBefore);
  gitIn(filtered, "ls-files", "-s");
});

test("an alternate GIT_INDEX_FILE selects the same files and is refreshed like the baseline", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n", "b.ts": "export const b = 1;\n" });
  writeFileSync(join(cwd, "b.ts"), wall);
  const later = new Date(Date.now() + 5000);
  utimesSync(join(cwd, "a.ts"), later, later);

  const alternate = join(scratch("alternate-index"), "index");
  writeFileSync(alternate, readFileSync(join(cwd, ".git", "index")));
  const modified = () => statSync(alternate, { bigint: true }).mtimeNs;
  const before = modified();

  const plain = run({ cwd }, "--check", "--changed");
  const env = { ...process.env, GIT_INDEX_FILE: alternate };
  const relocated = run({ cwd, env }, "--check", "--changed");

  expect(plain.code).toBe(1);
  expect(plain.stdout).toContain("b.ts");
  expect(relocated).toEqual(plain);
  expect(modified()).not.toBe(before);
});

test("a scoped hook lists indexed files in one call that reads no HEAD", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  const edited = (name: string) =>
    JSON.stringify({
      cwd,
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      tool_input: { file_path: join(cwd, name) },
    });

  const scoped = ({ args }: Call, path: string) => ({ args: [...args, path] });

  expectWaves(traced(cwd, 0, ["hook"], edited("a.ts")), [located, scoped(cached, "a.ts")]);

  writeFileSync(join(cwd, "b.ts"), "export const b = 1;\n");
  expectWaves(traced(cwd, 0, ["hook"], edited("b.ts")), [
    located,
    scoped(indexedOrUntracked, "b.ts"),
  ]);

  gitIn(cwd, "update-index", "--index-version", "4");
  expectWaves(
    traced(cwd, 0, ["hook"], edited("a.ts")),
    [located, scoped(indexedOrUntracked, "a.ts")],
    [call("check-attr linguist-generated stanza -z --stdin", "a.ts|")],
  );

  gitIn(cwd, "update-index", "--index-version", "2");
  const tree = git(cwd, "rev-parse", "HEAD^{tree}");
  rmSync(join(cwd, ".git", "objects", tree.slice(0, 2), tree.slice(2).trim()));
  expectWaves(traced(cwd, 0, ["hook"], edited("a.ts")), [located, scoped(cached, "a.ts")]);
});

test("a --hunks hook lists untracked files only when the index misses a path or cannot be read", () => {
  const cwd = committed({ "a.ts": "export const a = 1;\n" });
  const edited = (name: string) =>
    JSON.stringify({
      cwd,
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      tool_input: { file_path: join(cwd, name) },
    });

  const scoped = ({ args }: Call, path: string) => ({ args: [...args, path] });
  const hunks = (name: string, code = 0) => traced(cwd, code, ["hook", "--hunks"], edited(name));

  expectWaves(hunks("a.ts"), [located, scoped(diffHead, "a.ts")]);

  writeFileSync(join(cwd, "b.ts"), "export const b = 1;\n");
  expectWaves(hunks("b.ts"), [located, scoped(diffHead, "b.ts"), scoped(untracked, "b.ts")]);

  gitIn(cwd, "update-index", "--index-version", "4");
  expectWaves(hunks("a.ts"), [located, scoped(diffHead, "a.ts"), scoped(untracked, "a.ts")]);

  gitIn(cwd, "update-index", "--index-version", "2");
  const tree = git(cwd, "rev-parse", "HEAD^{tree}");
  rmSync(join(cwd, ".git", "objects", tree.slice(0, 2), tree.slice(2).trim()));

  expectWaves(
    hunks("a.ts", 1),
    [located, scoped(diffHead, "a.ts")],
    [call("rev-parse --verify -q HEAD")],
    [scoped(untracked, "a.ts")],
  );

  const hunkDiff = call(
    "-c diff.suppressBlankEmpty=false --literal-pathspecs diff --unified=999999999 --text --submodule=short --no-color --no-ext-diff --no-textconv --no-renames HEAD -- a.ts",
  );

  const withExtension = (root: string, signature: string) => {
    expect(readFileSync(join(root, ".git", "index")).includes(signature)).toBe(true);
    writeFileSync(join(root, "a.ts"), "export {};\n\n");
    const event = JSON.stringify({
      cwd: root,
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      tool_input: { file_path: join(root, "a.ts") },
    });

    expectWaves(
      traced(root, 0, ["hook", "--hunks"], event),
      [located, scoped(diffHead, "a.ts")],
      [call("check-attr linguist-generated stanza filter -z --stdin", "a.ts|")],
      [hunkDiff],
    );

    expectWaves(traced(root, 0, ["hook"], event), [located, scoped(cached, "a.ts")]);
    expect(commands(root, 0, "--check", "a.ts")).toEqual([located]);
  };

  const untrackedCache = committed({ "a.ts": "export {};\n" });
  gitIn(untrackedCache, "config", "core.untrackedCache", "true");
  gitIn(untrackedCache, "status");
  withExtension(untrackedCache, "UNTR");

  const resolved = committed({ "a.ts": "export {};\n" });
  gitIn(resolved, "checkout", "-qb", "side");
  writeFileSync(join(resolved, "a.ts"), "export const side = 1;\n");
  gitIn(resolved, "commit", "-qam", "side");

  gitIn(resolved, "checkout", "-q", "-");
  writeFileSync(join(resolved, "a.ts"), "export const main = 1;\n");
  gitIn(resolved, "commit", "-qam", "main");
  expect(() => git(resolved, "merge", "-q", "side")).toThrow();
  expect(git(resolved, "ls-files", "-u", "a.ts")).not.toBe("");

  writeFileSync(join(resolved, "a.ts"), "export {};\n");
  gitIn(resolved, "add", "a.ts");
  gitIn(resolved, "commit", "-qm", "merged");
  withExtension(resolved, "REUC");
});
