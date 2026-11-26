import { afterAll, expect, test } from "bun:test";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import {
  colorless,
  git,
  gitShim,
  run,
  scratch,
  scratchGitRepository,
  stanzaBinary,
} from "./support.ts";

const spawnSync = Bun.spawnSync;

function discoveries(calls: string[]): number {
  return calls.filter((call) => call.split(" ").includes("--show-toplevel")).length;
}

type Environment = Record<string, string | undefined>;

function assignEnvironment(values: Environment): void {
  for (const [name, value] of Object.entries(values))
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
}

function withEnvironment<T>(values: Environment, body: () => T): T {
  const saved = Object.fromEntries(Object.keys(values).map((name) => [name, process.env[name]]));
  assignEnvironment(values);

  try {
    return body();
  } finally {
    assignEnvironment(saved);
  }
}

function isolatedGitEnvironment(home: string): Environment {
  const inherited = Object.keys(process.env).filter((name) => name.startsWith("GIT_"));
  return {
    ...Object.fromEntries(inherited.map((name) => [name, undefined])),
    HOME: home,
    XDG_CONFIG_HOME: join(home, ".config"),
  };
}

function succeed(command: string[]): string {
  const result = spawnSync(command, { env: process.env });
  if (result.exitCode !== 0)
    throw new Error(`${command.join(" ")}: ${new TextDecoder().decode(result.stderr)}`);

  return new TextDecoder().decode(result.stdout);
}

function failureOf(body: () => unknown): unknown {
  try {
    body();
    return undefined;
  } catch (error) {
    return error;
  }
}

function throwFailures(failures: unknown[]): void {
  const thrown = failures.filter((failure) => failure !== undefined);
  if (thrown.length === 1) throw thrown[0];
  if (thrown.length > 1) throw new Error(thrown.map(String).join("\n"));
}

interface Mount {
  host?: string;
  mountpoint?: string;
  device?: string;
}

interface MountedVolume {
  host: string;
  root: string;
  start: string;
}

const mount: Mount = {};
function attachVolume(): MountedVolume {
  const host = realpathSync(mkdtempSync(join(tmpdir(), "stanza-mount-")));
  mount.host = host;

  const root = join(host, "repo");
  const mountpoint = join(root, "mnt");
  mkdirSync(mountpoint, { recursive: true });
  git(root, "init", "-q");

  mount.mountpoint = mountpoint;

  if (process.platform === "darwin") {
    const image = join(host, "volume.dmg");
    succeed(["hdiutil", "create", "-size", "2m", "-fs", "APFS", "-quiet", image]);
    const attached = succeed(["hdiutil", "attach", "-nobrowse", "-mountpoint", mountpoint, image]);
    mount.device = /\/dev\/disk\d+/.exec(attached)?.[0];
  } else if (process.platform === "linux") {
    const options = `size=2m,uid=${process.getuid?.()},gid=${process.getgid?.()}`;
    succeed(["sudo", "-n", "mount", "-t", "tmpfs", "-o", options, "tmpfs", mountpoint]);
  } else throw new Error(`the mount cases cannot attach a filesystem on ${process.platform}`);

  if (statSync(mountpoint).dev === statSync(root).dev)
    throw new Error(`nothing mounted at ${mountpoint}`);

  const start = join(mountpoint, "inner");
  mkdirSync(start);
  return { host, root, start };
}

type Attachment = { volume: MountedVolume } | { failure: unknown };

let attachment: Attachment | undefined;
function mountedVolume(): MountedVolume {
  attachment ??= attachOnce();
  if ("failure" in attachment) throw attachment.failure;
  return attachment.volume;
}

function attachOnce(): Attachment {
  try {
    return { volume: attachVolume() };
  } catch (failure) {
    return { failure };
  }
}

function detachVolume(mountpoint: string): void {
  const target = mount.device ?? mountpoint;
  const attempts =
    process.platform === "darwin"
      ? [
          ["hdiutil", "detach", "-quiet", target],
          ["hdiutil", "detach", "-quiet", "-force", target],
        ]
      : [
          ["sudo", "-n", "umount", mountpoint],
          ["sudo", "-n", "umount", "-l", mountpoint],
        ];

  const mounted = () => statSync(mountpoint).dev !== statSync(dirname(mountpoint)).dev;

  let failure: unknown;
  for (const command of attempts) {
    if (!mounted()) return;
    failure = failureOf(() => succeed(command));
  }

  if (mounted()) throw failure;
}

afterAll(() => {
  const { host, mountpoint } = mount;
  const detached = failureOf(() => mountpoint !== undefined && detachVolume(mountpoint));
  const removed = failureOf(
    () => host !== undefined && rmSync(host, { recursive: true, force: true }),
  );

  throwFailures([detached, removed]);
}, 60_000);

function checkProcesses(home: string, cwd: string): { code: number; spawns: number } {
  return withEnvironment(isolatedGitEnvironment(home), () => {
    const { code, spawns } = invoked(cwd, ["--check", "a.ts"]).result;
    return { code, spawns };
  });
}

interface Invoked {
  result: { code: number; spawns: number; stdout: string; stderr: string };
  calls: string[];
}

function invoked(cwd: string, argv: string[], input = ""): Invoked {
  const stdin = new TextEncoder().encode(input);
  const env = { ...process.env, AGENT_HOOKS: "1" };
  const shim = gitShim();
  const result = run(
    { cwd, stdin, env: { ...env, PATH: `${shim.bin}:${process.env.PATH ?? ""}` } },
    ...argv,
  );

  const calls = shim.calls();
  return { result: { ...result, spawns: calls.length }, calls };
}

function hookProcesses(
  home: string,
  cwd: string,
  input: unknown,
): { code: number; spawns: number } {
  return withEnvironment(isolatedGitEnvironment(home), () => {
    const { code, spawns, stdout, stderr } = invoked(cwd, ["hook"], JSON.stringify(input)).result;
    expect({ stdout, stderr }).toEqual({ stdout: "", stderr: "" });
    return { code, spawns };
  });
}

function writtenTranscript(files: string[]): string {
  const path = join(scratch("transcript"), "session.jsonl");
  const content = files.map((file_path, index) => ({
    type: "tool_use",
    id: `toolu_${index}`,
    name: "Write",
    input: { file_path, content: "export const value = 2;\n" },
  }));

  writeFileSync(path, `${JSON.stringify({ type: "assistant", message: { content } })}\n`);
  return path;
}

function stopInput(cwd: string, files: string[]): unknown {
  return { cwd, hook_event_name: "Stop", transcript_path: writtenTranscript(files) };
}

function editedRepository(directories: number): { cwd: string; files: string[] } {
  const names = Array.from(
    { length: 20 },
    (_, index) => `src/group${index % directories}/file${index}.ts`,
  );

  const cwd = scratchGitRepository({
    files: Object.fromEntries(names.map((name) => [name, "export const value = 1;\n"])),
    staged: true,
  });

  git(cwd, "commit", "-qm", "init");

  const files = names.map((name) => join(cwd, name));
  for (const file of files) writeFileSync(file, "export const value = 2;\n");
  return { cwd, files };
}

function linkedWorktree(files: Record<string, string>): { root: string; worktree: string } {
  const root = scratchGitRepository({ files, staged: true });
  git(root, "commit", "-qm", "init");

  const worktree = join(scratch(), "worktree");
  git(root, "worktree", "add", "--quiet", "--detach", worktree);
  return { root, worktree };
}

test("core.worktree naming the root in a CRLF config needs no discovery spawn", () => {
  const root = scratchGitRepository({ files: { "a.ts": "export const a = 1;\n" }, staged: true });
  const config = join(root, ".git", "config");
  const home = scratch();
  git(root, "config", "core.worktree", root);

  const checked = () =>
    withEnvironment(isolatedGitEnvironment(home), () => {
      const { result, calls } = invoked(root, ["--check", "a.ts"]);
      return { code: result.code, stdout: result.stdout, calls };
    });

  const plain = { code: 0, stdout: "", calls: [`-C ${root} var -l`] };
  expect(checked()).toEqual(plain);

  writeFileSync(config, readFileSync(config, "utf8").replaceAll("\n", "\r\n"));
  expect(checked()).toEqual(plain);
});

const needsBraces = "if (ready) {\n  act();\n}\n";

function attributedRepository(): string {
  const root = scratchGitRepository({
    files: {
      ".gitattributes": "sub/gen.ts linguist-generated\nsub/opt.ts -stanza\n",
      ".gitignore": "sub/ignored.ts\n",
      ...Object.fromEntries(
        ["a", "gen", "opt", "ignored"].map((name) => [`sub/${name}.ts`, needsBraces]),
      ),
    },
  });

  git(root, "add", ".");
  git(root, "commit", "-qm", "init");
  return root;
}

function withGitDir<T>(gitDir: string, body: () => T): T {
  return withEnvironment({ ...isolatedGitEnvironment(scratch()), GIT_DIR: gitDir }, body);
}

function rewritten(checkout: string): string[] {
  return ["a", "gen", "opt"].filter(
    (name) => readFileSync(join(checkout, `sub/${name}.ts`), "utf8") !== needsBraces,
  );
}

test("an absolute GIT_DIR fixes only what the root .gitattributes leaves in", () => {
  const root = attributedRepository();
  const { result } = withGitDir(join(root, ".git"), () =>
    invoked(root, ["--fix", "sub/a.ts", "sub/gen.ts", "sub/opt.ts"]),
  );

  expect(result.code).toBe(0);
  expect(rewritten(root)).toEqual(["a"]);
});

test("a pre-commit hook in a linked worktree fixes only that worktree's selected file", () => {
  const root = attributedRepository();
  const worktree = join(scratch(), "worktree");
  git(root, "worktree", "add", "--quiet", "--detach", worktree);

  const seen = join(scratch(), "git-dir");
  const errors = join(scratch(), "stderr");
  const hooks = join(root, ".git", "hooks");
  const stanza = stanzaBinary();
  writeFileSync(
    join(hooks, "pre-commit"),
    `#!/bin/sh\nprintf '%s' "$GIT_DIR" > '${seen}'\n'${stanza}' --fix sub/a.ts sub/gen.ts sub/opt.ts 2> '${errors}'\nexit 0\n`,
    { mode: 0o755 },
  );

  writeFileSync(join(worktree, "note.txt"), "note\n");
  withEnvironment(isolatedGitEnvironment(scratch()), () => {
    git(worktree, "add", "note.txt");
    git(worktree, { env: colorless() }, "-c", `core.hooksPath=${hooks}`, "commit", "-qm", "note");
  });

  expect(readFileSync(seen, "utf8")).toBe(join(root, ".git", "worktrees", "worktree"));
  expect(readFileSync(errors, "utf8")).toBe("rewrote 1 file, no findings left\n");
  expect(rewritten(worktree)).toEqual(["a"]);
  expect(rewritten(root)).toEqual([]);
});

test("a relative GIT_DIR checks a file in a subdirectory", () => {
  const root = attributedRepository();
  const { result } = withGitDir(".git", () => invoked(root, ["--check", "sub/a.ts"]));

  expect(result.code).toBe(1);
  expect(result.stdout).toContain("sub/a.ts:1:12: braces: ");
  expect(result.stderr).toBe("1 finding in 1 file, fixable with --fix\n");
});

test("an absolute GIT_DIR skips a file the root .gitignore ignores", () => {
  const root = attributedRepository();
  const { result } = withGitDir(join(root, ".git"), () => invoked(root, ["--check", "sub"]));

  expect(result.code).toBe(1);
  expect(result.stdout).toContain("sub/a.ts");
  expect(result.stdout).not.toContain("ignored.ts");
});

test.each([
  ["GIT_WORK_TREE", "."],
  ["GIT_COMMON_DIR", ".git"],
  ["GIT_OBJECT_DIRECTORY", ".git/objects"],
])("a relative %s without GIT_DIR keeps the root .gitattributes", (name, value) => {
  const root = attributedRepository();
  const environment = { ...isolatedGitEnvironment(scratch()), [name]: value };
  const { result } = withEnvironment(environment, () =>
    invoked(root, ["--check", "sub/a.ts", "sub/gen.ts"]),
  );

  expect(result.code).toBe(1);
  expect(result.stdout).toStartWith("sub/a.ts:1:12: braces: ");
  expect(result.stdout).not.toContain("gen.ts");
  expect(result.stderr).toBe("1 finding in 1 file, fixable with --fix\n");
});

test("relative git variables from below the work tree read the index git reads", () => {
  const root = scratchGitRepository({
    files: { "sub/a.ts": "export const value = 1;\n", "sub/b.ts": "export const value = 1;\n" },
    staged: true,
  });

  git(root, "commit", "-qm", "init");
  for (const name of ["a", "b"]) writeFileSync(join(root, `sub/${name}.ts`), needsBraces);
  git(root, "add", "sub/a.ts");
  writeFileSync(join(root, ".git", "alternate"), readFileSync(join(root, ".git", "index")));
  withEnvironment({ GIT_INDEX_FILE: ".git/alternate" }, () => {
    git(root, "reset", "-q", "--", "sub/a.ts");
    git(root, "add", "sub/b.ts");
  });

  const environment = {
    ...isolatedGitEnvironment(scratch()),
    GIT_DIR: "../.git",
    GIT_WORK_TREE: "..",
    GIT_INDEX_FILE: ".git/alternate",
  };

  const { result } = withEnvironment(environment, () =>
    invoked(join(root, "sub"), ["--check", "--staged"]),
  );

  expect(result.code).toBe(1);
  expect(result.stdout).toStartWith("b.ts:1:12: braces: ");
  expect(result.stdout).not.toContain("a.ts");
});

test("GIT_DIR costs one rev-parse beside the calls git discovery makes", () => {
  const root = attributedRepository();
  const argv = ["--check", "sub/a.ts"];
  const without = withEnvironment(isolatedGitEnvironment(scratch()), () => invoked(root, argv));
  const pinned = withGitDir(join(root, ".git"), () => invoked(root, argv));

  expect(without.calls.filter((call) => !call.startsWith("-C "))).toEqual([]);
  expect(pinned.calls.filter((call) => !call.startsWith("-C "))).toEqual([
    "rev-parse --show-toplevel",
  ]);
});

function committedCheckout(root: string): string {
  mkdirSync(root, { recursive: true });
  git(root, "init", "-q");

  writeFileSync(join(root, "a.ts"), "export {};\n");
  git(root, "add", "a.ts");
  git(root, "commit", "-qm", "init");
  return realpathSync(root);
}

function expectOnlyVariables(cwd: string): void {
  const { result, calls } = withEnvironment(isolatedGitEnvironment(scratch()), () =>
    invoked(cwd, ["--check", "a.ts"]),
  );

  expect(result).toEqual({
    code: 0,
    spawns: 1,
    stdout: "",
    stderr: "checked 1 file, no findings\n",
  });

  expect(calls).toEqual([`-C ${cwd} var -l`]);
}

const rootNames = [
  ["ascii", "ascii"],
  ["NFC", "caf\u00e9"],
  ["NFD", "cafe\u0301"],
  ["CJK", "\u65e5\u672c"],
];

test.each(rootNames)("an %s root is located in process with git's bytes", (_, name) => {
  expectOnlyVariables(committedCheckout(join(scratch(), name)));
});

test("a linked worktree of a UTF-8 repository is located in process", () => {
  const parent = scratch();
  const main = committedCheckout(join(parent, "\u65e5\u672c"));
  const linked = join(parent, "linked");
  git(main, "worktree", "add", "--quiet", "--detach", linked);

  expectOnlyVariables(linked);
});

test("a UTF-8 separate git directory is followed in process", () => {
  const parent = scratch();
  const work = join(parent, "work");
  git(parent, "init", "-q", "--separate-git-dir", join(parent, "\u65e5\u672c-git"), work);

  writeFileSync(join(work, "a.ts"), "export {};\n");
  git(work, "add", "a.ts");
  git(work, "commit", "-qm", "init");

  expectOnlyVariables(work);
});

test.if(process.env.CI !== undefined)("CI mounts a filesystem for the discovery tests", () => {
  const { root, start } = mountedVolume();
  expect(statSync(start).dev).not.toBe(statSync(root).dev);
});

test("a directory in no repository is outside without a git process", () => {
  const cwd = scratch();
  const file = join(cwd, "x.ts");
  writeFileSync(file, "export const value = 1;\n");
  const write = {
    cwd,
    hook_event_name: "PostToolUse",
    tool_name: "Write",
    tool_input: { file_path: file, content: "export const value = 1;\n" },
  };

  withEnvironment(isolatedGitEnvironment(scratch("home")), () => {
    expect(invoked(cwd, ["--check", "x.ts"]).result).toMatchObject({ code: 0, spawns: 0 });
    expect(invoked(cwd, ["hook"], JSON.stringify(write)).result).toMatchObject({
      code: 0,
      spawns: 0,
    });
  });
});

test("one file check with attributes uses two git processes without root discovery", () => {
  const cwd = scratchGitRepository({
    files: {
      "a.ts": "export const value = 1;\n",
      ".gitattributes": "*.unused linguist-generated\n",
    },
  });

  const { result, calls } = invoked(cwd, ["--check", "a.ts"]);
  expect({ code: result.code, spawns: result.spawns }).toEqual({ code: 0, spawns: 2 });
  expect(discoveries(calls)).toBe(0);
});

test("Stop hook with attributes uses three git processes without root discovery", () => {
  const files = Object.fromEntries(
    Array.from({ length: 20 }, (_, index) => [
      `src/group${index % 4}/file${index}.ts`,
      "export const value = 1;\n",
    ]),
  );

  const cwd = scratchGitRepository({
    files: { ...files, ".gitattributes": "*.unused linguist-generated\n" },
    staged: true,
  });

  git(cwd, "commit", "-qm", "init");

  const written = Object.keys(files).map((file) => join(cwd, file));
  for (const file of written) writeFileSync(file, "export const value = 2;\n");

  const { result, calls } = invoked(cwd, ["hook"], JSON.stringify(stopInput(cwd, written)));
  expect(result).toEqual({ code: 0, spawns: 3, stdout: "", stderr: "" });
  expect(discoveries(calls)).toBe(0);
  for (const file of written) expect(readFileSync(file, "utf8")).toBe("export const value = 2;\n");
});

test("one file check without attribute sources spawns 1 git process, 2 before 147", () => {
  const cwd = scratchGitRepository({ files: { "a.ts": "export const value = 1;\n" } });
  expect(checkProcesses(scratch(), cwd)).toEqual({ code: 0, spawns: 1 });
});

test("one file check with a global include spawns 1 git process, 2 before 147", () => {
  const cwd = scratchGitRepository({ files: { "a.ts": "export const value = 1;\n" } });
  const home = scratch();
  writeFileSync(join(home, "included.config"), "[user]\n\tname = t\n");
  writeFileSync(join(home, ".gitconfig"), `[include]\n\tpath = ${join(home, "included.config")}\n`);

  expect(checkProcesses(home, cwd)).toEqual({ code: 0, spawns: 1 });
});

test("PostToolUse Edit without attribute sources spawns 2 git processes, 4 before 147", () => {
  const { cwd, files } = editedRepository(3);
  const input = {
    cwd,
    hook_event_name: "PostToolUse",
    tool_name: "Edit",
    tool_input: { file_path: files[0] },
  };

  expect(hookProcesses(scratch(), cwd, input)).toEqual({ code: 0, spawns: 2 });
});

function hookCalls(home: string, cwd: string, input: unknown): string[] {
  return withEnvironment(isolatedGitEnvironment(home), () => {
    const { result, calls } = invoked(cwd, ["hook"], JSON.stringify(input));
    expect(result).toMatchObject({ code: 0, stdout: "", stderr: "" });
    return calls;
  });
}

function checkoutPair(): { main: string; worktree: string } {
  const { root, worktree } = linkedWorktree({
    "src/a.ts": "export const value = 1;\n",
    ".gitattributes": "*.unused text\n",
    "src/.gitattributes": "*.md -diff\n",
  });

  for (const checkout of [root, worktree])
    writeFileSync(join(checkout, "src/a.ts"), "export const value = 2;\n");

  return { main: root, worktree };
}

test("PostToolUse Edit in a linked worktree spawns what the main checkout spawns", () => {
  const { main, worktree } = checkoutPair();
  const spawned = (cwd: string) =>
    hookCalls(scratch(), cwd, {
      cwd,
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      tool_input: { file_path: join(cwd, "src/a.ts") },
    })
      .map((call) => call.replace(cwd, "<checkout>"))
      .sort();

  const inMain = spawned(main);
  expect(inMain).toHaveLength(2);
  expect(spawned(worktree)).toEqual(inMain);
});

function stdinCalls(home: string, cwd: string): string[] {
  return withEnvironment(isolatedGitEnvironment(home), () => {
    const { result, calls } = invoked(
      cwd,
      ["--fix", "--stdin", "src/a.ts"],
      "export const value = 3;\n",
    );

    expect(result).toMatchObject({ code: 0, stderr: "" });
    return calls;
  });
}

test("--stdin in a linked worktree spawns no check-attr", () => {
  const { worktree } = checkoutPair();
  const calls = stdinCalls(scratch(), worktree);

  expect(calls.filter((call) => call.includes("check-attr"))).toEqual([]);
  expect(discoveries(calls)).toBe(0);
});

test("--stdin in a linked worktree asks git when the common info/attributes may apply", () => {
  const { main, worktree } = checkoutPair();
  writeFileSync(join(main, ".git", "info", "attributes"), "*.ts linguist-generated\n");
  const calls = stdinCalls(scratch(), worktree);

  expect(calls.filter((call) => call.includes("check-attr"))).toHaveLength(1);
});

test("Stop with 20 files in three directories spawns 2 git processes, 23 before 147", () => {
  const { cwd, files } = editedRepository(3);
  expect(hookProcesses(scratch(), cwd, stopInput(cwd, files))).toEqual({ code: 0, spawns: 2 });
});
