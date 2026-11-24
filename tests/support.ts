import { afterEach } from "bun:test";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readlinkSync,
  realpathSync,
  renameSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import type { BatchRequest, BatchResponse } from "../scripts/batch.ts";
import { countSpawned, cpuOf } from "./budget.ts";

const appleGitShim = "/usr/bin/git";

export const gitLinks = join(import.meta.dir, "..", "target", "test-git");

function linkGit(links: string, target: string): void {
  const link = join(links, "git");
  mkdirSync(links, { recursive: true });
  if (linkTarget(link) === target) return;

  const staged = `${link}.${process.pid}`;
  rmSync(staged, { force: true });
  symlinkSync(target, staged);
  renameSync(staged, link);
}

function linkTarget(link: string): string | undefined {
  try {
    return readlinkSync(link);
  } catch {
    return undefined;
  }
}

export function chooseGit(path: string, links = gitLinks): { binary: string; path: string } {
  const found = Bun.which("git", { PATH: path });
  if (process.platform !== "darwin" || !found || realpathSync(found) !== appleGitShim)
    return { binary: found ?? "git", path };

  const result = Bun.spawnSync(["/usr/bin/xcrun", "--find", "git"], { env: process.env });
  const real = result.exitCode === 0 ? result.stdout.toString().trim() : "";
  if (!real) return { binary: found, path };

  linkGit(links, real);
  return { binary: real, path: `${links}:${path}` };
}

const pathAtStart = process.env.PATH ?? "";
const chosenGit = chooseGit(pathAtStart);
if (chosenGit.path !== pathAtStart) process.env.PATH = chosenGit.path;

export const gitBinary = chosenGit.binary;

interface PathSelection {
  files: string[];
  errors: string[];
  warnings: string[];
  submodules: string[];
  optedOut: boolean;
}

export type Location =
  | { kind: "repository"; root: string }
  | { kind: "outside" }
  | { kind: "failed"; error: string };

const directories: string[] = [];

afterEach(() => {
  for (const path of directories.splice(0)) rmSync(path, { recursive: true, force: true });
});

export const findingRow = /^(.+):(\d+):(\d+): ([a-z-]+): (.+?)( \(by hand\))?$/;

export function scratch(suffix = ""): string {
  const path = realpathSync(mkdtempSync(join(tmpdir(), `stanza-${suffix ? `${suffix}-` : ""}`)));
  directories.push(path);
  return path;
}

interface ScratchGitRepositoryOptions {
  files?: Record<string, string>;
  staged?: boolean | string[];
}

export function scratchGitRepository(options: ScratchGitRepositoryOptions = {}): string {
  const cwd = scratch("repo");
  git(cwd, "init", "-q");

  for (const [name, text] of Object.entries(options.files ?? {})) {
    const path = join(cwd, name);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, text);
  }

  if (options.staged) {
    const staged = Array.isArray(options.staged)
      ? options.staged
      : Object.keys(options.files ?? {});

    git(cwd, "add", "--", ...staged);
  }

  return cwd;
}

const identity = {
  GIT_AUTHOR_NAME: "t",
  GIT_AUTHOR_EMAIL: "t@t",
  GIT_COMMITTER_NAME: "t",
  GIT_COMMITTER_EMAIL: "t@t",
};

interface GitOptions {
  env?: NodeJS.ProcessEnv;
  stdin?: Uint8Array;
}

export function git(cwd: string, options: GitOptions, ...args: string[]): string;
export function git(cwd: string, ...args: string[]): string;
export function git(cwd: string, first?: GitOptions | string, ...rest: string[]): string {
  const options = typeof first === "object" ? first : {};
  const args = typeof first === "string" ? [first, ...rest] : rest;

  const result = Bun.spawnSync([gitBinary, "-c", "commit.gpgsign=false", ...args], {
    cwd,
    env: { ...(options.env ?? process.env), ...identity },
    stdin: options.stdin,
  });

  if (result.exitCode !== 0) throw new Error(result.stderr.toString());
  return result.stdout.toString();
}

export function gitIn(cwd: string, ...args: string[]): void {
  git(cwd, ...args);
}

export function superproject(files: Record<string, string>): string {
  const sub = scratchGitRepository({ files: { "s.ts": "f();\n".repeat(7) }, staged: true });
  git(sub, "commit", "-qm", "init");

  const cwd = scratchGitRepository({ files });
  git(cwd, "-c", "protocol.file.allow=always", "submodule", "add", "-q", sub, "vendor/sub");
  git(cwd, "commit", "-qm", "init");
  return cwd;
}

export function rot13FilterProcess(aborting = ""): string {
  const script = join(import.meta.dir, "rot13-filter.ts");
  return `'${process.execPath}' '${script}' '${aborting}'`;
}

export function hunkFunction(name: string, value = 2): string {
  return `function ${name}(a: boolean) {\n  if (a) {\n    return 1;\n  }\n  return ${value};\n}`;
}

export const wall = `export function f() {\n${"  step();\n".repeat(6)}}\n`;
export const unfixed = Buffer.from(wall.replace("{\n", "{\n\n"));

export function committedSource(source: string): string {
  const cwd = scratchGitRepository({ files: { "a.ts": source }, staged: true });
  git(cwd, "commit", "-qm", "init");
  return cwd;
}

export const agentVariables = [
  "CLAUDECODE",
  "AI_AGENT",
  "CODEX_SANDBOX",
  "CODEX_THREAD_ID",
  "CURSOR_AGENT",
  "GEMINI_CLI",
];

const colorVariables = ["FORCE_COLOR", "NO_COLOR", ...agentVariables];

export function colorless(env: NodeJS.ProcessEnv = process.env): NodeJS.ProcessEnv {
  return Object.fromEntries(Object.entries(env).filter(([key]) => !colorVariables.includes(key)));
}

interface RunOptions {
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  stdin?: Uint8Array;
}

interface RunResult {
  code: number;
  stderr: string;
  stdout: string;
}

export function stanzaBinary(): string {
  const configured = process.env.STANZA_BIN;
  const binary = configured
    ? resolve(configured)
    : join(import.meta.dir, "..", "target", "release", "stanza");

  if (!existsSync(binary))
    throw new Error(
      configured
        ? `STANZA_BIN names no file: ${binary}`
        : `no binary at ${binary}: run cargo build --release, or set STANZA_BIN`,
    );

  return binary;
}

export function devBinary(): string {
  const binary = join(dirname(stanzaBinary()), "stanza-dev");
  if (!existsSync(binary)) throw new Error(`no stanza-dev beside the binary: ${binary}`);
  return binary;
}

function devCommand(name: string): string[] {
  return [devBinary(), name];
}

function dev(
  cwd: string,
  [name, ...args]: [string, ...string[]],
  options: { env?: NodeJS.ProcessEnv; stdin?: string } = {},
): string[] {
  const stdin = options.stdin === undefined ? undefined : new TextEncoder().encode(options.stdin);
  const { result, cpuMs } = spawned(
    [...devCommand(name), ...args],
    { cwd, stdin },
    colorless(options.env),
  );

  countSpawned(cpuMs);
  if (result.code !== 0) throw new Error(`stanza-dev ${name} failed: ${result.stderr}`);
  return result.stdout.split("\n").filter((line) => line !== "");
}

export function batchCommand(): string[] {
  return devCommand("batch");
}

export function batch(cwd: string, requests: BatchRequest[]): BatchResponse[] {
  const stdin = requests.map((request, id) => `${JSON.stringify({ id, ...request })}\n`).join("");
  return dev(cwd, ["batch"], { stdin }).map((line) => JSON.parse(line) as BatchResponse);
}

type Finding = BatchResponse["findings"][number];

interface Formatted {
  code: number;
  text: string;
  findings: Finding[];
}

export function format(
  path: string,
  text: string,
  options: { mode: "check" | "fix"; braces?: "on" | "off"; cwd?: string },
): Formatted {
  const braces =
    options.braces === undefined ? [] : [options.braces === "on" ? "--braces" : "--no-braces"];

  const result = run(
    { cwd: options.cwd, stdin: new TextEncoder().encode(text) },
    `--${options.mode}`,
    "--json",
    ...braces,
    "--stdin",
    path,
  );

  const fixing = options.mode === "fix";

  let findings: Finding[];
  try {
    findings = JSON.parse(fixing ? result.stderr : result.stdout) as Finding[];
  } catch {
    throw new Error(`stanza exited ${result.code}: ${result.stderr}`);
  }

  return { code: result.code, text: fixing ? result.stdout : text, findings };
}

export function collectFiles(paths: string[], cwd: string, keepGenerated = false): PathSelection {
  const flags = keepGenerated ? ["--keep-generated"] : [];
  return JSON.parse(dev(cwd, ["select", ...flags, "--", ...paths])[0]!) as PathSelection;
}

interface Located {
  locations: Location[];
  calls: string[];
}

export function locateTraced(dirs: string[]): Located {
  const shim = gitShim();
  const env = { ...process.env, PATH: `${shim.bin}:${process.env.PATH ?? ""}` };
  const locations = dev(dirs[0] ?? process.cwd(), ["locate", ...dirs], { env }).map(
    (line) => JSON.parse(line) as Location,
  );

  return { locations, calls: shim.calls() };
}

interface GitShim {
  bin: string;
  calls: () => string[];
}

export function gitShim(): GitShim {
  const bin = scratch("git-shim");
  const log = join(bin, "calls.log");
  const executable = Bun.which(gitBinary) ?? gitBinary;
  writeFileSync(log, "");
  writeFileSync(
    join(bin, "git"),
    `#!/bin/sh\nprintf '%s\\n' "$*" >> '${log}'\nexec '${executable}' "$@"\n`,
    { mode: 0o755 },
  );

  const calls = () =>
    readFileSync(log, "utf8")
      .split("\n")
      .filter((line) => line !== "");

  return { bin, calls };
}

export function gitFailing(pattern: string, stderr: string, code: number): NodeJS.ProcessEnv {
  const bin = scratch("failing-git");
  const executable = Bun.which("git", { PATH: process.env.PATH }) ?? gitBinary;
  writeFileSync(
    join(bin, "git"),
    `#!/bin/sh\ncase "$*" in ${pattern}) echo "${stderr}" >&2; exit ${code};; esac\nexec "${executable}" "$@"\n`,
    { mode: 0o755 },
  );

  return { ...process.env, PATH: `${bin}:${process.env.PATH ?? ""}` };
}

export function gitWithoutAttrSource(): NodeJS.ProcessEnv {
  return gitFailing("*--attr-source=*", "unknown option", 129);
}

export function tracedGit<T>(body: (env: NodeJS.ProcessEnv) => T): { result: T; calls: string[] } {
  const shim = gitShim();
  const result = body({ ...process.env, PATH: `${shim.bin}:${process.env.PATH ?? ""}` });
  return { result, calls: shim.calls() };
}

function spawned(
  command: string[],
  options: RunOptions,
  env: NodeJS.ProcessEnv,
): { result: RunResult; cpuMs: number } {
  const result = Bun.spawnSync(command, { ...options, env, timeout: 120_000 });
  if (result.exitedDueToTimeout) throw new Error(`${command[0]} ran past 120 seconds`);
  if (result.signalCode) throw new Error(`${command[0]} was killed by ${result.signalCode}`);

  const decoder = new TextDecoder();
  return {
    result: {
      code: result.exitCode ?? 0,
      stderr: decoder.decode(result.stderr),
      stdout: decoder.decode(result.stdout),
    },
    cpuMs: cpuOf(result),
  };
}

export function run(options: RunOptions, ...args: string[]): RunResult;
export function run(...args: string[]): RunResult;
export function run(first?: RunOptions | string, ...rest: string[]): RunResult {
  const options = typeof first === "object" ? first : {};
  const args = typeof first === "string" ? [first, ...rest] : rest;

  const { result, cpuMs } = spawned(
    [stanzaBinary(), ...args],
    { cwd: options.cwd ?? process.cwd(), stdin: options.stdin ?? new Uint8Array() },
    colorless(options.env),
  );

  countSpawned(cpuMs);
  return result;
}
