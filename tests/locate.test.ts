import { afterAll, afterEach, beforeEach, expect, spyOn, test } from "bun:test";
import {
  appendFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { main } from "../src/cli.ts";
import { beginInvocation } from "../src/directories.ts";
import { locate, type Location } from "../src/files.ts";
import {
  devBinary,
  gitBinary,
  gitShim,
  locateTraced,
  scratch,
  scratchGitRepository,
  stanzaCommand,
} from "./support.ts";

const spawnSync = Bun.spawnSync;

function watchDiscovery() {
  return spyOn(Bun, "spawnSync");
}

let spawn: ReturnType<typeof watchDiscovery>;

beforeEach(() => {
  beginInvocation();
  spawn = watchDiscovery();
});

afterEach(() => {
  spawn.mockRestore();
});

const binary = devBinary() !== undefined;

function trace(): string[] {
  return spawn.mock.calls.flatMap(([args]) =>
    Array.isArray(args) && typeof args[0] === "string" && /(?:^|\/)git$/.test(args[0])
      ? [args.slice(1).join(" ")]
      : [],
  );
}

function discoveries(calls: string[]): number {
  return calls.filter((call) => call.split(" ").includes("--show-toplevel")).length;
}

function discoveryCalls(): number {
  return discoveries(trace());
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
  beginInvocation();

  const { locations, calls } = locateTraced([dir], trace);
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
  if (!binary) {
    let stdout = "";
    let stderr = "";
    const before = trace().length;
    const code = main(argv, {
      cwd,
      env,
      stdin: () => stdin,
      stdout: (chunk) => {
        stdout += chunk;
      },
      stderr: (chunk) => {
        stderr += chunk;
      },
    });

    if (typeof code !== "number") throw new Error("a one file run started a pooled run");

    const calls = trace().slice(before);
    return { result: { code, spawns: calls.length, stdout, stderr }, calls };
  }

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

  const first = locateTraced([root, nested], trace);
  expect(first.locations).toEqual([located, located]);
  expect(configCalls(first.calls)).toBe(1);

  beginInvocation();
  const second = locateTraced([nested], trace);
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

test("worktree git file falls back to git", () => {
  const root = scratchGitRepository({ files: { "a.ts": "export const value = 1;\n" } });
  commit(root);

  const worktree = join(scratch(), "worktree");
  git(root, "worktree", "add", "--quiet", "--detach", worktree);

  expect(expectLocation(worktree, 1)).toEqual({ kind: "repository", root: worktree });
});

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
    expect(expectLocation(root, 1)).toEqual({ kind: "repository", root });
  } finally {
    if (saved === undefined) delete process.env.GIT_DIR;
    else process.env.GIT_DIR = saved;
  }
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

test.skipIf(binary)("repository owned by another user goes to git", () => {
  const root = scratchGitRepository();
  const owner = spyOn(process, "geteuid").mockReturnValue((process.geteuid?.() ?? 0) + 1);
  try {
    expect(expectLocation(root, 1)).toEqual({ kind: "repository", root });
  } finally {
    owner.mockRestore();
  }
});

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
  beginInvocation();

  const { locations, calls } = locateTraced([root, root], trace);
  expect(locations).toEqual([expected, expected]);
  expect(discoveries(calls)).toBe(1);
});

test.skipIf(binary)("new invocation sees a directory that became a repository", () => {
  const root = scratch();
  expect(expectLocation(root, 1, "outside")).toEqual({ kind: "outside" });

  git(root, "init", "-q");
  expect(locate(root)).toEqual({ kind: "outside" });
  expect(discoveryCalls()).toBe(1);

  expect(expectLocation(root, 0)).toEqual({ kind: "repository", root });
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

test("Stop hook with attributes uses four git processes without root discovery", () => {
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
  expect(result).toEqual({ code: 0, spawns: 4, stdout: "", stderr: "" });
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

test("PostToolUse Edit without attribute sources spawns 3 git processes, 4 before 147", () => {
  const { cwd, files } = editedRepository(3);
  const input = {
    cwd,
    hook_event_name: "PostToolUse",
    tool_name: "Edit",
    tool_input: { file_path: files[0] },
  };

  expect(hookProcesses(scratch(), cwd, input)).toEqual({ code: 0, spawns: 3 });
});

test("Stop with 20 files in three directories spawns 3 git processes, 23 before 147", () => {
  const { cwd, files } = editedRepository(3);
  expect(hookProcesses(scratch(), cwd, stopInput(cwd, files))).toEqual({ code: 0, spawns: 3 });
});
