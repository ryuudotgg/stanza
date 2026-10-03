import { afterEach, beforeEach, expect, spyOn, test } from "bun:test";
import {
  appendFileSync,
  mkdirSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { main, runHookCall, type Io } from "../src/cli.ts";
import { beginInvocation } from "../src/directories.ts";
import { locate, type Location } from "../src/files.ts";
import { gitBinary, scratch, scratchGitRepository } from "./support.ts";

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

function discoveryCalls(): number {
  return spawn.mock.calls.filter(
    ([args]) => Array.isArray(args) && args.includes("--show-toplevel"),
  ).length;
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

  const root = new TextDecoder().decode(result.stdout).trim();
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

  const before = discoveryCalls();
  expect(locate(dir)).toEqual(expected);
  expect(discoveryCalls() - before).toBe(count);
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

  expect(locate(root)).toEqual({ kind: "repository", root });
  expect(locate(nested)).toEqual({ kind: "repository", root });
  const configCalls = () =>
    spawn.mock.calls.filter(([args]) => Array.isArray(args) && args.includes("var")).length;

  expect(configCalls()).toBe(1);

  beginInvocation();
  expect(locate(nested)).toEqual({ kind: "repository", root });
  expect(configCalls()).toBe(2);
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

test("included config falls back to git", () => {
  const root = scratchGitRepository();
  const worktree = scratch();
  const config = join(scratch(), "included.config");
  writeFileSync(config, `[core]\n\tworktree = ${worktree}\n`);
  git(root, "config", "include.path", config);

  expectLocation(root, 1);
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

  expect(locate(root)).toEqual(expected);
  expect(locate(root)).toEqual(expected);
  expect(discoveryCalls()).toBe(1);
});

test("new invocation sees a directory that became a repository", () => {
  const root = scratch();
  expect(expectLocation(root, 1, "outside")).toEqual({ kind: "outside" });

  git(root, "init", "-q");
  expect(locate(root)).toEqual({ kind: "outside" });
  expect(discoveryCalls()).toBe(1);

  expect(expectLocation(root, 0)).toEqual({ kind: "repository", root });
});

test("one file check in a plain repository uses no discovery spawns", () => {
  const cwd = scratchGitRepository({ files: { "a.ts": "export const value = 1;\n" } });
  const io: Io = {
    cwd,
    env: process.env,
    stdin: () => new Uint8Array(),
    stdout: () => {},
    stderr: () => {},
  };

  expect(main(["--check", "a.ts"], io)).toBe(0);
  expect(discoveryCalls()).toBe(0);
});

test("Stop hook on twenty written files uses no discovery spawns", () => {
  const files = Object.fromEntries(
    Array.from({ length: 20 }, (_, index) => [
      `src/group${index % 4}/file${index}.ts`,
      "export const value = 1;\n",
    ]),
  );

  const cwd = scratchGitRepository({ files });
  commit(cwd);

  const written = new Set(Object.keys(files).map((file) => join(cwd, file)));
  for (const file of written) writeFileSync(file, "export const value = 2;\n");

  let stdout = "";
  let stderr = "";
  const io: Io = {
    cwd,
    env: process.env,
    stdin: () => new Uint8Array(),
    stdout: (chunk) => {
      stdout += chunk;
    },
    stderr: (chunk) => {
      stderr += chunk;
    },
  };

  expect(runHookCall({ event: "stop", cwd, written }, [], io)).toBe(0);
  expect(stdout).toBe("");
  expect(stderr).toBe("");
  expect(discoveryCalls()).toBe(0);
  for (const file of written) expect(readFileSync(file, "utf8")).toBe("export const value = 2;\n");
});
