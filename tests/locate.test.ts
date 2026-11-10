import { afterAll, expect, test } from "bun:test";
import {
  appendFileSync,
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import {
  gitBinary,
  gitShim,
  locateTraced,
  type Location,
  scratch,
  scratchGitRepository,
  stanzaCommand,
} from "./support.ts";

const spawnSync = Bun.spawnSync;

function discoveries(calls: string[]): number {
  return calls.filter((call) => call.split(" ").includes("--show-toplevel")).length;
}

function git(cwd: string, ...args: string[]): void {
  const result = spawnSync([gitBinary, "-C", cwd, ...args], { env: process.env });
  expect(new TextDecoder().decode(result.stderr)).toBe("");
  expect(result.exitCode).toBe(0);
}

function gitLocation(dir: string, failureKind: "outside" | "failed" = "failed"): Location {
  const result = spawnSync([gitBinary, "-C", dir, "rev-parse", "--show-toplevel"], {
    env: process.env,
  });

  const root = new TextDecoder().decode(result.stdout).replace(/\n$/, "");
  if (result.exitCode === 0 && root) return { kind: "repository", root };
  if (failureKind === "outside") return { kind: "outside" };

  const reason =
    new TextDecoder()
      .decode(result.stderr)
      .split("\n")
      .map((line) => line.trim())
      .find(Boolean) ?? `exit ${result.exitCode}`;

  return { kind: "failed", error: `git rev-parse failed in ${dir}: ${reason}` };
}

function expectLocation(
  dir: string,
  count: number,
  failureKind: "outside" | "failed" = "failed",
): Location {
  const expected = gitLocation(dir, failureKind);

  const { locations, calls } = locateTraced([dir]);
  expect(locations).toEqual([expected]);
  expect(discoveries(calls)).toBe(count);
  return expected;
}

function commit(cwd: string): void {
  git(cwd, "add", ".");
  git(
    cwd,
    "-c",
    "commit.gpgsign=false",
    "-c",
    "user.email=t@t",
    "-c",
    "user.name=t",
    "commit",
    "-qm",
    "init",
  );
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

const passwordlessSudo = failureOf(() => succeed(["sudo", "-n", "true"])) === undefined;

function refusesForeignOwner(): boolean {
  const root = scratchGitRepository();
  const home = scratch();
  const env = Object.fromEntries(
    Object.entries({
      ...process.env,
      ...isolatedGitEnvironment(home),
      GIT_TEST_ASSUME_DIFFERENT_OWNER: "1",
    }).filter((entry): entry is [string, string] => entry[1] !== undefined),
  );

  const result = spawnSync([gitBinary, "-C", root, "rev-parse", "--show-toplevel"], { env });
  return (
    result.exitCode !== 0 && new TextDecoder().decode(result.stderr).includes("dubious ownership")
  );
}

const gitRefusesForeignOwner = refusesForeignOwner();

interface Volume {
  host?: string;
  root?: string;
  mountpoint?: string;
  device?: string;
  start?: string;
}

const volume: Volume = {};
function attachVolume(): void {
  if (process.platform !== "darwin" && !(process.platform === "linux" && passwordlessSudo)) return;

  volume.host = realpathSync(mkdtempSync(join(tmpdir(), "stanza-mount-")));
  const root = join(volume.host, "repo");
  const mountpoint = join(root, "mnt");
  volume.root = root;
  volume.mountpoint = mountpoint;

  mkdirSync(mountpoint, { recursive: true });
  succeed([gitBinary, "-C", root, "init", "-q"]);

  if (process.platform === "darwin") {
    const image = join(volume.host, "volume.dmg");
    succeed(["hdiutil", "create", "-size", "2m", "-fs", "APFS", "-quiet", image]);
    const attached = succeed(["hdiutil", "attach", "-nobrowse", "-mountpoint", mountpoint, image]);
    volume.device = /\/dev\/disk\d+/.exec(attached)?.[0];
  } else {
    const options = `size=2m,uid=${process.getuid?.()},gid=${process.getgid?.()}`;
    succeed(["sudo", "-n", "mount", "-t", "tmpfs", "-o", options, "tmpfs", mountpoint]);
  }

  if (statSync(mountpoint).dev === statSync(root).dev)
    throw new Error(`nothing mounted at ${mountpoint}`);

  mkdirSync(join(mountpoint, "inner"));
  volume.start = join(mountpoint, "inner");
}

function detachVolume(mountpoint: string): void {
  const target = volume.device ?? mountpoint;
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

const volumeFailure = failureOf(attachVolume);

afterAll(() => {
  const { host, mountpoint } = volume;
  const detached = failureOf(() => mountpoint !== undefined && detachVolume(mountpoint));
  const removed = failureOf(
    () => host !== undefined && rmSync(host, { recursive: true, force: true }),
  );

  throwFailures([detached, removed]);
}, 60_000);

function innerRepositoryStart(outer: string, damage: (marker: string) => void): string {
  const inner = join(outer, "inner");
  mkdirSync(inner);
  git(inner, "init", "-q");
  damage(join(inner, ".git"));

  const start = join(inner, "sub");
  mkdirSync(start);
  return start;
}

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
  const result = spawnSync([...stanzaCommand(), ...argv], {
    cwd,
    stdin,
    env: { ...env, PATH: `${shim.bin}:${process.env.PATH ?? ""}` },
  });

  const decoder = new TextDecoder();
  const calls = shim.calls();
  return {
    result: {
      code: result.exitCode ?? -1,
      spawns: calls.length,
      stdout: decoder.decode(result.stdout),
      stderr: decoder.decode(result.stderr),
    },
    calls,
  };
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
  });

  commit(cwd);

  const files = names.map((name) => join(cwd, name));
  for (const file of files) writeFileSync(file, "export const value = 2;\n");
  return { cwd, files };
}

test("plain root and nested directory match git without discovery spawns", () => {
  const root = scratchGitRepository();
  const nested = join(root, "src", "nested");
  mkdirSync(nested, { recursive: true });

  expect(expectLocation(root, 0)).toEqual({ kind: "repository", root });
  expect(expectLocation(nested, 0)).toEqual({ kind: "repository", root });
});

test("nested repository takes precedence without a discovery spawn", () => {
  const outer = scratchGitRepository();
  const inner = join(outer, "inner");
  mkdirSync(inner);
  git(inner, "init", "-q");

  expect(expectLocation(inner, 0)).toEqual({ kind: "repository", root: inner });
});

test("config validation runs once per repository per invocation", () => {
  const root = scratchGitRepository();
  const nested = join(root, "nested");
  mkdirSync(nested);

  const located: Location = { kind: "repository", root };
  const configCalls = (calls: string[]) =>
    calls.filter((call) => call.split(" ").includes("var")).length;

  const first = locateTraced([root, nested]);
  expect(first.locations).toEqual([located, located]);
  expect(configCalls(first.calls)).toBe(1);

  const second = locateTraced([nested]);
  expect(second.locations).toEqual([located]);
  expect(configCalls(second.calls)).toBe(1);
});

test("symlinked directory discovers the real repository root", () => {
  const root = scratchGitRepository();
  const nested = join(root, "nested");
  const link = join(scratch(), "linked");
  mkdirSync(nested);
  symlinkSync(nested, link);

  expect(expectLocation(link, 0)).toEqual({ kind: "repository", root });
});

function linkedWorktree(files: Record<string, string> = { "a.ts": "export const value = 1;\n" }): {
  root: string;
  worktree: string;
  gitdir: string;
} {
  const root = scratchGitRepository({ files });
  commit(root);

  const worktree = join(scratch(), "worktree");
  git(root, "worktree", "add", "--quiet", "--detach", worktree);
  return { root, worktree, gitdir: join(root, ".git", "worktrees", "worktree") };
}

function submoduleCheckout(): { root: string; checkout: string; gitdir: string } {
  const source = scratchGitRepository({ files: { "a.ts": "export const value = 1;\n" } });
  commit(source);

  const root = scratchGitRepository();
  git(
    root,
    "-c",
    "protocol.file.allow=always",
    "submodule",
    "add",
    "--quiet",
    source,
    "vendor/sub",
  );

  return {
    root,
    checkout: join(root, "vendor", "sub"),
    gitdir: join(root, ".git", "modules", "vendor", "sub"),
  };
}

test("a linked worktree is located without a discovery spawn", () => {
  const { worktree } = linkedWorktree();
  const nested = join(worktree, "src");
  mkdirSync(nested);

  expect(expectLocation(worktree, 0)).toEqual({ kind: "repository", root: worktree });
  expect(expectLocation(nested, 0)).toEqual({ kind: "repository", root: worktree });
});

test("a submodule checkout is located without a discovery spawn", () => {
  const { checkout } = submoduleCheckout();
  expect(expectLocation(checkout, 0)).toEqual({ kind: "repository", root: checkout });
});

test("a submodule whose core.worktree names another directory falls back to git", () => {
  const { checkout, gitdir } = submoduleCheckout();
  const elsewhere = scratch();
  git(gitdir, "config", "core.worktree", elsewhere);

  expect(expectLocation(checkout, 1)).toEqual({ kind: "repository", root: elsewhere });
});

test("core.worktree naming the root needs no discovery spawn", () => {
  const root = scratchGitRepository();
  const start = join(root, "sub");
  mkdirSync(start);
  appendFileSync(join(root, ".git", "config"), `[core]\n\tworktree = ${root}\n`);

  expect(expectLocation(start, 0)).toEqual({ kind: "repository", root });
});

test.skipIf(process.platform !== "darwin")(
  "core.worktree naming the root in another letter case matches git",
  () => {
    const parent = scratch();
    const root = join(parent, "Repo");
    const start = join(root, "sub");
    mkdirSync(start, { recursive: true });

    git(root, "init", "-q");
    git(root, "config", "core.worktree", join(parent, "REPO"));

    expect(expectLocation(start, 1)).toEqual({ kind: "repository", root: join(parent, "REPO") });
  },
);

test("a linked worktree of a submodule ignores the shared core.worktree like git", () => {
  const { checkout } = submoduleCheckout();
  const worktree = join(scratch(), "worktree");
  git(checkout, "worktree", "add", "--quiet", "--detach", worktree);

  expect(expectLocation(worktree, 0)).toEqual({ kind: "repository", root: worktree });
});

const splitWorktreeValues: [string, (root: string) => string][] = [
  ["a raw U+2028", (root) => `${root} x`],
  ["an escaped line break", (root) => `"${root}\\nfoo.bar"`],
];

test.each(splitWorktreeValues)(
  "core.worktree naming the root before %s matches git",
  (_, value) => {
    const root = scratchGitRepository();
    const start = join(root, "sub");
    mkdirSync(start);
    appendFileSync(join(root, ".git", "config"), `[core]\n\tworktree = ${value(root)}\n`);

    expectLocation(start, 1);
  },
);

test("a linked worktree of a bare clone ignores the shared core.bare like git", () => {
  const source = scratchGitRepository({ files: { "a.ts": "export const value = 1;\n" } });
  commit(source);

  const bare = join(scratch(), "bare.git");
  git(source, "clone", "--quiet", "--bare", source, bare);
  const worktree = join(scratch(), "worktree");
  git(bare, "worktree", "add", "--quiet", "--detach", worktree);

  expect(expectLocation(worktree, 0)).toEqual({ kind: "repository", root: worktree });
});

const brokenGitFiles: [string, (worktree: string, gitdir: string) => void][] = [
  ["missing", (_, gitdir) => rmSync(gitdir, { recursive: true })],
  ["without HEAD", (_, gitdir) => rmSync(join(gitdir, "HEAD"))],
  [
    "named by a line that is not gitdir",
    (worktree) => writeFileSync(join(worktree, ".git"), "x\n"),
  ],
  [
    "sharing a missing common dir",
    (_, gitdir) => writeFileSync(join(gitdir, "commondir"), "../x\n"),
  ],
];

test.each(brokenGitFiles)("a .git file whose gitdir is %s falls back to git", (_, damage) => {
  const { worktree, gitdir } = linkedWorktree();
  damage(worktree, gitdir);
  expectLocation(worktree, 1);
});

test.skipIf(process.getuid?.() === 0)(
  "a .git file whose gitdir is unreadable falls back to git",
  () => {
    const { worktree, gitdir } = linkedWorktree();
    chmodSync(gitdir, 0);

    const asserted = failureOf(() => expectLocation(worktree, 1));
    chmodSync(gitdir, 0o755);
    throwFailures([asserted]);
  },
);

test.skipIf(!passwordlessSudo || !gitRefusesForeignOwner)(
  "a .git file whose gitdir another user owns fails like git",
  () => {
    const { worktree, gitdir } = linkedWorktree();
    const user = process.geteuid?.() ?? 0;
    succeed(["chmod", "-R", "a+rX", gitdir]);
    succeed(["sudo", "-n", "chown", "-R", String(user + 1), gitdir]);

    const asserted = failureOf(() =>
      withEnvironment(isolatedGitEnvironment(scratch()), () => {
        expect(expectLocation(worktree, 1)).toMatchObject({
          kind: "failed",
          error: expect.stringContaining("dubious ownership"),
        });
      }),
    );

    const restored = failureOf(() => succeed(["sudo", "-n", "chown", "-R", String(user), gitdir]));

    throwFailures([asserted, restored]);
  },
  60_000,
);

test("git directory missing HEAD falls back to git", () => {
  const root = scratchGitRepository();
  rmSync(join(root, ".git", "HEAD"));
  expect(expectLocation(root, 1).kind).toBe("failed");
});

test("path inside git directory falls back to git", () => {
  const root = scratchGitRepository();
  expect(expectLocation(join(root, ".git"), 1, "outside")).toEqual({ kind: "outside" });
});

test("GIT_DIR defined at call time falls back to git", () => {
  const root = scratchGitRepository();
  const saved = process.env.GIT_DIR;
  try {
    process.env.GIT_DIR = join(root, ".git");
    expect(expectLocation(root, 2)).toEqual({ kind: "repository", root });
  } finally {
    if (saved === undefined) delete process.env.GIT_DIR;
    else process.env.GIT_DIR = saved;
  }
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

  commit(root);
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
  const hooks = join(root, ".git", "hooks");
  const stanza = stanzaCommand().join("' '");
  writeFileSync(
    join(hooks, "pre-commit"),
    `#!/bin/sh\nprintf '%s' "$GIT_DIR" > '${seen}'\n'${stanza}' --fix sub/a.ts sub/gen.ts sub/opt.ts\nexit 0\n`,
    { mode: 0o755 },
  );

  writeFileSync(join(worktree, "note.txt"), "note\n");
  withEnvironment(isolatedGitEnvironment(scratch()), () => {
    git(worktree, "add", "note.txt");
    git(
      worktree,
      "-c",
      `core.hooksPath=${hooks}`,
      "-c",
      "commit.gpgsign=false",
      "-c",
      "user.email=t@t",
      "-c",
      "user.name=t",
      "commit",
      "-qm",
      "note",
    );
  });

  expect(readFileSync(seen, "utf8")).toBe(join(root, ".git", "worktrees", "worktree"));
  expect(rewritten(worktree)).toEqual(["a"]);
  expect(rewritten(root)).toEqual([]);
});

test("a relative GIT_DIR checks a file in a subdirectory", () => {
  const root = attributedRepository();
  const { result } = withGitDir(".git", () => invoked(root, ["--check", "sub/a.ts"]));

  expect(result.code).toBe(1);
  expect(result.stdout).toContain("sub/a.ts:1:12 braces");
  expect(result.stderr).toBe("");
});

test("an absolute GIT_DIR skips a file the root .gitignore ignores", () => {
  const root = attributedRepository();
  const { result } = withGitDir(join(root, ".git"), () => invoked(root, ["--check", "sub"]));

  expect(result.code).toBe(1);
  expect(result.stdout).toContain("sub/a.ts");
  expect(result.stdout).not.toContain("ignored.ts");
});

test("relative git variables from below the work tree read the index git reads", () => {
  const root = scratchGitRepository({
    files: { "sub/a.ts": "export const value = 1;\n", "sub/b.ts": "export const value = 1;\n" },
  });

  commit(root);
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
  expect(result.stdout).toStartWith("b.ts:1:12 braces");
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

test("core.worktree falls back to git's configured root", () => {
  const root = scratchGitRepository();
  const worktree = scratch();
  git(root, "config", "core.worktree", worktree);

  expect(expectLocation(root, 1)).toEqual({ kind: "repository", root: worktree });
});

test("core.bare true falls back to git's failure", () => {
  const root = scratchGitRepository();
  git(root, "config", "core.bare", "true");
  expect(expectLocation(root, 1).kind).toBe("failed");
});

test("valueless core.bare falls back to git's failure", () => {
  const root = scratchGitRepository();
  appendFileSync(join(root, ".git", "config"), "[core]\n\tbare\n");
  expect(expectLocation(root, 1).kind).toBe("failed");
});

test("included config without discovery keys needs no discovery spawn", () => {
  const root = scratchGitRepository();
  const config = join(scratch(), "included.config");
  writeFileSync(config, "[user]\n\tname = t\n");
  git(root, "config", "include.path", config);

  expect(expectLocation(root, 0)).toEqual({ kind: "repository", root });
});

const worktreeIncludes: [string, (root: string, config: string) => void][] = [
  ["include", (root, config) => git(root, "config", "include.path", config)],
  [
    "includeIf gitdir",
    (root, config) => git(root, "config", `includeIf.gitdir:${root}/.git.path`, config),
  ],
  [
    "includeIf onbranch",
    (root, config) => {
      git(root, "symbolic-ref", "HEAD", "refs/heads/main");
      git(root, "config", "includeIf.onbranch:main.path", config);
    },
  ],
  [
    "includeIf hasconfig",
    (root, config) => {
      git(root, "config", "remote.origin.url", "https://example.com/x.git");
      appendFileSync(
        join(root, ".git", "config"),
        `[includeIf "hasconfig:remote.*.url:https://example.com/**"]\n\tpath = ${config}\n`,
      );
    },
  ],
];

test.each(worktreeIncludes)("%s setting core.worktree matches git's root", (_, include) => {
  const root = scratchGitRepository();
  const config = join(scratch(), "included.config");
  writeFileSync(config, `[core]\n\tworktree = ${scratch()}\n`);
  include(root, config);

  expect(expectLocation(root, 1)).toEqual({ kind: "repository", root });
});

test("a root ending in a space keeps the space on both discovery paths", () => {
  const root = join(scratch(), "trailing ");
  mkdirSync(root);
  git(root, "init", "-q");

  expect(expectLocation(root, 0)).toEqual({ kind: "repository", root });

  withEnvironment({ GIT_CONFIG_COUNT: "0" }, () => {
    expect(expectLocation(root, 1)).toEqual({ kind: "repository", root });
  });
});

test("GIT_CEILING_DIRECTORIES stops discovery where git stops", () => {
  const root = scratchGitRepository();
  const start = join(root, "a", "b");
  mkdirSync(start, { recursive: true });

  withEnvironment({ GIT_CEILING_DIRECTORIES: join(root, "a") }, () => {
    expect(expectLocation(start, 1, "outside")).toEqual({ kind: "outside" });
  });
});

test("GIT_WORK_TREE makes git's work tree the root", () => {
  const root = scratchGitRepository();
  const worktree = join(root, "sub");
  const start = join(worktree, "deeper");
  mkdirSync(start, { recursive: true });

  withEnvironment({ GIT_WORK_TREE: worktree }, () => {
    expect(expectLocation(start, 1)).toEqual({ kind: "repository", root: worktree });
  });
});

test.skipIf(!gitRefusesForeignOwner)(
  "GIT_TEST_ASSUME_DIFFERENT_OWNER fails with git's ownership error",
  () => {
    const root = scratchGitRepository();
    const environment = {
      ...isolatedGitEnvironment(scratch()),
      GIT_TEST_ASSUME_DIFFERENT_OWNER: "1",
    };

    withEnvironment(environment, () => {
      expect(expectLocation(root, 1)).toMatchObject({
        kind: "failed",
        error: expect.stringContaining("dubious ownership"),
      });
    });
  },
);

test.skipIf(!passwordlessSudo || !gitRefusesForeignOwner)(
  "repository chowned to another user fails like git",
  () => {
    const root = scratchGitRepository();
    const user = process.geteuid?.() ?? 0;
    succeed(["chmod", "-R", "a+rX", root]);
    succeed(["sudo", "-n", "chown", "-R", String(user + 1), root]);

    const asserted = failureOf(() =>
      withEnvironment(isolatedGitEnvironment(scratch()), () => {
        expect(expectLocation(root, 1)).toMatchObject({
          kind: "failed",
          error: expect.stringContaining("dubious ownership"),
        });
      }),
    );

    const restored = failureOf(() => succeed(["sudo", "-n", "chown", "-R", String(user), root]));
    throwFailures([asserted, restored]);
  },
  60_000,
);

test.skipIf(volume.start === undefined)(
  "mounted filesystem stops discovery where git stops",
  () => {
    expect(expectLocation(volume.start!, 1)).toMatchObject({
      kind: "failed",
      error: expect.stringContaining("not a git repository"),
    });
  },
);

test.skipIf(volume.start === undefined)(
  "GIT_DISCOVERY_ACROSS_FILESYSTEM crosses a mounted filesystem like git",
  () => {
    withEnvironment({ GIT_DISCOVERY_ACROSS_FILESYSTEM: "1" }, () => {
      expect(expectLocation(volume.start!, 1)).toEqual({ kind: "repository", root: volume.root! });
    });
  },
);

test.skipIf(volume.start === undefined)(
  "a mounted filesystem with no repository above is outside without discovery",
  () => {
    const marker = join(volume.root!, ".git");
    const aside = join(volume.host!, "git-aside");
    renameSync(marker, aside);

    try {
      expect(expectLocation(volume.start!, 0, "outside")).toEqual({ kind: "outside" });
      withEnvironment({ GIT_DISCOVERY_ACROSS_FILESYSTEM: "1" }, () => {
        expect(expectLocation(volume.start!, 1, "outside")).toEqual({ kind: "outside" });
      });
    } finally {
      renameSync(aside, marker);
    }
  },
);

test.if(process.env.CI !== undefined)("CI mounts a filesystem for the discovery tests", () => {
  if (volumeFailure !== undefined) throw volumeFailure;
  expect(volume.start).toBeDefined();
});

test("GIT_DISCOVERY_ACROSS_FILESYSTEM without a mount needs no discovery spawn", () => {
  const root = scratchGitRepository();
  const nested = join(root, "nested");
  mkdirSync(nested);

  withEnvironment({ GIT_DISCOVERY_ACROSS_FILESYSTEM: "1" }, () => {
    expect(expectLocation(nested, 0)).toEqual({ kind: "repository", root });
  });
});

test("inner git directory without objects defers to the outer repository like git", () => {
  const outer = scratchGitRepository();
  const start = innerRepositoryStart(outer, (marker) =>
    rmSync(join(marker, "objects"), { recursive: true }),
  );

  expect(expectLocation(start, 1)).toEqual({ kind: "repository", root: outer });
});

test("inner git directory without refs defers to the outer repository like git", () => {
  const outer = scratchGitRepository();
  const start = innerRepositoryStart(outer, (marker) =>
    rmSync(join(marker, "refs"), { recursive: true }),
  );

  expect(expectLocation(start, 1)).toEqual({ kind: "repository", root: outer });
});

test("inner git directory with an invalid HEAD defers to the outer repository like git", () => {
  const outer = scratchGitRepository();
  const start = innerRepositoryStart(outer, (marker) =>
    writeFileSync(join(marker, "HEAD"), "garbage\n"),
  );

  expect(expectLocation(start, 1)).toEqual({ kind: "repository", root: outer });
});

test("environment config falls back to git", () => {
  const root = scratchGitRepository();
  const worktree = scratch();
  const names = ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"];
  const saved = names.map((name) => process.env[name]);
  try {
    process.env.GIT_CONFIG_COUNT = "1";
    process.env.GIT_CONFIG_KEY_0 = "core.worktree";
    process.env.GIT_CONFIG_VALUE_0 = worktree;
    expectLocation(root, 1);
  } finally {
    for (const [index, name] of names.entries())
      if (saved[index] === undefined) delete process.env[name];
      else process.env[name] = saved[index];
  }
});

test("malformed config falls back to git's exact error", () => {
  const root = scratchGitRepository();
  appendFileSync(join(root, ".git", "config"), "not a config line\n");
  expect(expectLocation(root, 1).kind).toBe("failed");
});

test("invalid core boolean falls back to git's exact error", () => {
  const root = scratchGitRepository();
  git(root, "config", "core.filemode", "banana");
  expect(expectLocation(root, 1).kind).toBe("failed");
});

test("unsupported repository format falls back to git's exact error", () => {
  const root = scratchGitRepository();
  git(root, "config", "core.repositoryformatversion", "2");
  expect(expectLocation(root, 1).kind).toBe("failed");
});

test("invalid global config falls back to git's exact error", () => {
  const root = scratchGitRepository();
  const home = scratch();
  writeFileSync(join(home, ".gitconfig"), "not a config line\n");
  const saved = process.env.HOME;
  try {
    process.env.HOME = home;
    expect(expectLocation(root, 1).kind).toBe("failed");
  } finally {
    if (saved === undefined) delete process.env.HOME;
    else process.env.HOME = saved;
  }
});

test("fallback location is memoized for the exact directory string", () => {
  const root = scratchGitRepository();
  appendFileSync(join(root, ".git", "config"), "not a config line\n");
  const expected = gitLocation(root);

  const { locations, calls } = locateTraced([root, root]);
  expect(locations).toEqual([expected, expected]);
  expect(discoveries(calls)).toBe(1);
});

test("a directory in no repository is outside without a git process", () => {
  const cwd = scratch();
  const file = join(cwd, "x.ts");
  writeFileSync(file, "export const value = 1;\n");
  const write = {
    cwd,
    hook_event_name: "PreToolUse",
    tool_name: "Write",
    tool_input: { file_path: file, content: "export const value = 2;\n" },
  };

  withEnvironment(isolatedGitEnvironment(scratch("home")), () => {
    expect(expectLocation(cwd, 0, "outside")).toEqual({ kind: "outside" });
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
  });

  commit(cwd);

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

test("PreToolUse Write in a linked worktree spawns no check-attr", () => {
  const { worktree } = checkoutPair();
  const file = join(worktree, "src/a.ts");
  const calls = hookCalls(scratch(), worktree, {
    cwd: worktree,
    hook_event_name: "PreToolUse",
    tool_name: "Write",
    tool_input: { file_path: file, content: "export const value = 3;\n" },
  });

  expect(calls.filter((call) => call.includes("check-attr"))).toEqual([]);
  expect(discoveries(calls)).toBe(0);
});

test("PreToolUse Write in a linked worktree asks git when the common info/attributes may apply", () => {
  const { main, worktree } = checkoutPair();
  writeFileSync(join(main, ".git", "info", "attributes"), "*.ts linguist-generated\n");
  const file = join(worktree, "src/a.ts");
  const calls = hookCalls(scratch(), worktree, {
    cwd: worktree,
    hook_event_name: "PreToolUse",
    tool_name: "Write",
    tool_input: { file_path: file, content: "export const value = 3;\n" },
  });

  expect(calls.filter((call) => call.includes("check-attr"))).toHaveLength(1);
});

test("Stop with 20 files in three directories spawns 2 git processes, 23 before 147", () => {
  const { cwd, files } = editedRepository(3);
  expect(hookProcesses(scratch(), cwd, stopInput(cwd, files))).toEqual({ code: 0, spawns: 2 });
});
