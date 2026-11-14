import { afterEach, expect } from "bun:test";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import type { BatchRequest, BatchResponse } from "../scripts/batch.ts";
import { countSpawned, cpuOf } from "./budget.ts";

function preferRealGit(): string {
  if (process.platform !== "darwin") return "git";

  const result = Bun.spawnSync(["xcrun", "--find", "git"]);
  if (result.exitCode !== 0) return "git";

  const gitPath = new TextDecoder().decode(result.stdout).trim();
  if (!gitPath) return "git";

  const dir = mkdtempSync(join(tmpdir(), "stanza-git-"));
  symlinkSync(gitPath, join(dir, "git"));
  process.env.PATH = `${dir}:${process.env.PATH}`;
  process.on("exit", () => rmSync(dir, { recursive: true, force: true }));
  return gitPath;
}

export const gitBinary = preferRealGit();

export interface Collected {
  files: string[];
  errors: string[];
  warnings: string[];
  submodules: string[];
}

export interface PathSelection extends Collected {
  optedOut: boolean;
}

export interface ChangedLines {
  lines: Set<number>;
  deletedAfter: Set<number>;
}

export type Location =
  | { kind: "repository"; root: string }
  | { kind: "outside" }
  | { kind: "failed"; error: string };

export type StdinTarget =
  | { status: "format"; path: string; root?: string }
  | { status: "skip"; reason: "opted out" | "excluded" }
  | { status: "unsupported"; error: string }
  | { status: "failed"; error: string };

const directories: string[] = [];

afterEach(() => {
  for (const path of directories.splice(0)) rmSync(path, { recursive: true, force: true });
});

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
  const result = Bun.spawnSync([gitBinary, "init", "-q"], { cwd });
  if (result.exitCode !== 0) throw new Error(new TextDecoder().decode(result.stderr));

  for (const [name, text] of Object.entries(options.files ?? {})) {
    const path = join(cwd, name);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, text);
  }

  if (options.staged) {
    const staged = Array.isArray(options.staged)
      ? options.staged
      : Object.keys(options.files ?? {});

    const added = Bun.spawnSync([gitBinary, "add", "--", ...staged], { cwd });
    if (added.exitCode !== 0) throw new Error(new TextDecoder().decode(added.stderr));
  }

  return cwd;
}

export function rot13FilterProcess(aborting = ""): string {
  const script = join(import.meta.dir, "rot13-filter.ts");
  return `'${process.execPath}' '${script}' '${aborting}'`;
}

export function hunkFunction(name: string, value = 2): string {
  return `function ${name}(a: boolean) {\n  if (a) {\n    return 1;\n  }\n  return ${value};\n}`;
}

export function committedSource(source: string): string {
  const cwd = scratchGitRepository({ files: { "a.ts": source }, staged: true });
  const configured = Bun.spawnSync([gitBinary, "config", "commit.gpgsign", "false"], { cwd });
  expect(configured.exitCode).toBe(0);

  const result = Bun.spawnSync(
    [gitBinary, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init"],
    { cwd },
  );

  expect(result.exitCode).toBe(0);
  return cwd;
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

function parse(input: (RunOptions | string)[]): { options: RunOptions; args: string[] } {
  const [first] = input;
  const options = typeof first === "object" ? first : {};
  const args = input.filter((item): item is string => typeof item === "string");
  return { options, args };
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

export function dev(
  cwd: string,
  [name, ...args]: [string, ...string[]],
  options: { env?: NodeJS.ProcessEnv; stdin?: string } = {},
): string[] {
  const stdin = options.stdin === undefined ? undefined : new TextEncoder().encode(options.stdin);
  const { result, cpuMs } = spawned(
    [...devCommand(name), ...args],
    { cwd, stdin },
    {
      ...(options.env ?? process.env),
      FORCE_COLOR: undefined,
    },
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

export interface Formatted {
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

  const result = runMain([`--${options.mode}`, "--json", ...braces, "--stdin", path], {
    cwd: options.cwd ?? process.cwd(),
    stdin: new TextEncoder().encode(text),
  });

  const fixing = options.mode === "fix";

  let findings: Finding[];
  try {
    findings = JSON.parse(fixing ? result.stderr : result.stdout) as Finding[];
  } catch {
    throw new Error(`stanza exited ${result.code}: ${result.stderr}`);
  }

  return { code: result.code, text: fixing ? result.stdout : text, findings };
}

export interface Catalog {
  rules: Record<string, { fixable: boolean; gap: boolean; summary: string; message: string }>;
  flags: [string, string][];
  usage: string;
  claudeCodeHooks: string;
  extensions: string[];
}

let catalogued: Catalog | undefined;
export function catalog(): Catalog {
  return (catalogued ??= JSON.parse(dev(process.cwd(), ["catalog"]).join("\n")) as Catalog);
}

export interface BraceDecision {
  family: string;
  setting: string;
  files: string[];
}

export function braceDecisions(cwd: string, dirs: string[], extension = ""): BraceDecision[][] {
  const real = realpathSync(cwd);
  return dev(cwd, ["braces", "--extension", extension, ...dirs]).map((line) =>
    (JSON.parse(line) as { decisions: Record<string, BraceDecision[]> }).decisions[extension]!.map(
      (decision) => ({
        ...decision,
        files: decision.files.map((file) => resolve(real, file)),
      }),
    ),
  );
}

export function bracesEnforced(dir: string, extension?: string): boolean {
  const [decisions] = braceDecisions(process.cwd(), [dir], extension);
  return decisions!.some((decision) => decision.setting !== "off");
}

export interface WidthSource {
  kind: "config" | "default" | "fallback";
  file?: string;
  formatter?: string;
}

export interface Width {
  columns: number;
  tab: number;
  source: WidthSource;
  unread: string[];
}

export function formatterWidth(path: string): Width {
  const real = realpathSync(process.cwd());
  const width = JSON.parse(dev(process.cwd(), ["width", path])[0]!) as Width;
  const source = width.source.file
    ? { ...width.source, file: resolve(real, width.source.file) }
    : width.source;

  return { ...width, source, unread: width.unread.map((file) => resolve(real, file)) };
}

export function collectFiles(paths: string[], cwd: string, keepGenerated = false): PathSelection {
  const flags = keepGenerated ? ["--keep-generated"] : [];
  return JSON.parse(dev(cwd, ["select", ...flags, "--", ...paths])[0]!) as PathSelection;
}

export function collectChanged(
  cwd: string,
  hunks = false,
  written?: Set<string>,
): Collected & { changedLines: Map<string, ChangedLines> } {
  const flags = hunks ? ["--hunks"] : [];
  const paths = written ? ["--", ...written] : [];
  const { changedLines, ...selected } = JSON.parse(
    dev(cwd, ["changed", ...flags, ...paths])[0]!,
  ) as Collected & { changedLines: Record<string, { lines: number[]; deletedAfter: number[] }> };

  return {
    ...selected,
    changedLines: new Map(
      Object.entries(changedLines).map(([path, changed]) => [
        path,
        { lines: new Set(changed.lines), deletedAfter: new Set(changed.deletedAfter) },
      ]),
    ),
  };
}

export function stdinTarget(input: string, cwd: string): StdinTarget {
  return JSON.parse(dev(cwd, ["stdin-target", input])[0]!) as StdinTarget;
}

export interface Located {
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

export interface GitShim {
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

export function runMain(argv: string[], options: RunOptions = {}): RunResult {
  const env = Object.fromEntries(
    Object.entries(options.env ?? process.env).filter(([key]) => key !== "FORCE_COLOR"),
  );

  const { result, cpuMs } = spawned(
    [stanzaBinary(), ...argv],
    { cwd: options.cwd ?? process.cwd(), stdin: options.stdin ?? new Uint8Array() },
    env,
  );

  countSpawned(cpuMs);
  return result;
}

export function run(...input: (RunOptions | string)[]): RunResult {
  const { options, args } = parse(input);
  return runMain(args, options);
}
