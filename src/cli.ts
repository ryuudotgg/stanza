#!/usr/bin/env bun
import { version } from "../package.json" with { type: "json" };
import { readFileSync, realpathSync } from "node:fs";
import { dirname, relative, resolve } from "node:path";
import { beginInvocation } from "./directories.ts";
import {
  collectChanged,
  collectFiles,
  collectStaged,
  emptyReason,
  errorCode,
  ignoredByGit,
  isGeneratedHeader,
  landsWithin,
  locate,
  stdinTarget,
  trackedInHead,
  type EmptyReason,
  type Location,
  type StagedFile,
} from "./files.ts";
import { explain } from "./engine/explain.ts";
import { languageOf } from "./languages/index.ts";
import { blockReason, claudeCodeHooks, hookCall, rereadLine, type HookCall } from "./hook.ts";
import { systemIo, type Io } from "./start.ts";
import type { Changed } from "./engine/model.ts";
import { RULES } from "./engine/rules.ts";
import { decode, withoutMark } from "./step.ts";
import { type Braces, type Finding, type Mode } from "./engine/types.ts";
import { formatFile, mergeOutcomes, readText, type Formatted, type Input } from "./format.ts";
import { bothBraces, columns, flags, usage } from "./usage.ts";

export type { Io } from "./start.ts";

declare const STANZA_COMMIT: string | undefined;

interface Arguments {
  changed: boolean;
  hunks: boolean;
  json: boolean;
  mode: Mode;
  braces: Braces | undefined;
  paths: string[];
  staged: boolean;
  stdin: string | undefined;
}

interface ExplainArguments {
  path: string;
  line: number;
  noBraces: boolean;
}

interface Run {
  cwd: string;
  mode: Mode;
  braces: Braces | undefined;
  write: boolean;
  display?: (path: string) => string;
}

const switches = new Set(["--braces", "--changed", "--hunks", "--json", "--no-braces", "--staged"]);
const standalone = new Set(["--help", "-h", "--version"]);

function help(): string {
  const rules = Object.entries(RULES).map(([id, rule]) => [
    `  ${id}`,
    rule.fixable ? "fix" : "check",
    rule.summary,
  ]);

  return [
    usage,
    "",
    ...columns(flags),
    "",
    "Claude Code .claude/settings.json:",
    claudeCodeHooks,
    "",
    "Rules:",
    ...columns(rules),
  ].join("\n");
}

function parseExplain(args: string[]): ExplainArguments | { error: string } {
  const targets = args.filter((arg) => arg !== "--no-braces");
  if (args.length - targets.length > 1) return { error: "--no-braces given twice" };

  const unexpected = targets.find((arg) => arg.startsWith("-")) ?? targets[1];
  if (unexpected !== undefined) return { error: `unexpected argument ${unexpected}` };

  const match = /^(.+):(\d+)$/.exec(targets[0] ?? "");
  if (!match) return { error: "explain needs <file>:<line>" };

  const line = Number(match[2]);
  if (line < 1) return { error: `${targets[0]}: the line must be 1 or more` };

  return { path: match[1]!, line, noBraces: targets.length < args.length };
}

function bracesFlag(given: (flag: string) => boolean): Arguments["braces"] {
  if (given("--braces")) return "on";
  if (given("--no-braces")) return "off";
}

function parseArguments(args: string[]): Arguments | { error: string } {
  const paths: string[] = [];
  const seen = new Set<string>();
  const queue = args.values();

  let mode: Mode | undefined;
  let stdin: string | undefined;
  for (const arg of queue) {
    if (arg === "--") {
      paths.push(...queue);
      break;
    }

    if (arg === "--fix" || arg === "--check") {
      if (mode !== undefined) return { error: "use one of --fix or --check" };
      mode = arg.slice(2) as Mode;
      continue;
    }

    if (switches.has(arg)) {
      if (seen.has(arg)) return { error: `${arg} given twice` };
      seen.add(arg);
      continue;
    }

    if (arg === "--stdin") {
      const path = queue.next().value;
      if (stdin !== undefined) return { error: "--stdin given twice" };
      if (path === undefined || path.startsWith("-")) return { error: "--stdin needs a path" };

      stdin = path;
      continue;
    }

    if (standalone.has(arg)) return { error: `${arg} takes no other arguments` };
    if (arg.startsWith("-")) return { error: `unknown flag ${arg}` };

    paths.push(arg);
  }

  const changed = seen.has("--changed");
  const hunks = seen.has("--hunks");
  const staged = seen.has("--staged");
  const sources = [changed, staged, paths.length > 0, stdin !== undefined].filter(Boolean).length;
  if (mode === undefined) return { error: "--fix or --check is required" };
  if (sources === 0)
    return { error: "nothing to format: give paths, --changed, --staged or --stdin <path>" };

  if (sources > 1) return { error: "use only one of --changed, --staged, --stdin or paths" };
  if (staged && mode === "fix") return { error: "--staged works only with --check" };
  if (hunks && !changed && !staged) return { error: "--hunks needs --changed or --staged" };
  if (seen.has("--braces") && seen.has("--no-braces")) return { error: bothBraces };

  return {
    changed,
    hunks,
    json: seen.has("--json"),
    mode,
    braces: bracesFlag((flag) => seen.has(flag)),
    paths,
    staged,
    stdin,
  };
}

function printedPath(path: string, cwd: string): string {
  const output = relative(cwd, path);
  return output || path;
}

function printFindings(findings: Finding[], json: boolean, print: (line: string) => void): void {
  if (json) {
    print(JSON.stringify(findings));
    return;
  }

  for (const finding of findings)
    print(`${finding.path}:${finding.line}:${finding.col} ${finding.rule} ${finding.message}`);
}

function runStdin(input: string, args: Arguments, io: Io): number {
  let source: Uint8Array;
  try {
    source = io.stdin();
  } catch (error: unknown) {
    const message = `cannot read stdin: ${errorCode(error)}`;
    warn(io, `stanza: ${message}`);
    return status([message], { failed: false, findings: [] });
  }

  const target = stdinTarget(input, io.cwd);
  const inputs =
    target.status === "format" ? [{ path: target.path, read: () => decode(source) }] : [];

  const result = formatInputs(inputs, {
    cwd: io.cwd,
    mode: args.mode,
    braces: args.braces,
    write: false,
  });

  const fix = args.mode === "fix";
  if (fix)
    io.stdout((target.status === "format" ? result.kept.get(target.path) : undefined) ?? source);

  printFindings(result.findings, args.json, (line) => (fix ? io.stderr : io.stdout)(`${line}\n`));

  if (!(fix && args.json)) warnUnread(io, io.cwd, result.unread, result.unreadWidth);

  const errors =
    target.status === "unsupported" || target.status === "failed" ? [target.error] : [];

  for (const error of errors) warn(io, `stanza: ${error}`);

  return status(errors, result);
}

function runExplain(argv: string[], io: Io): number {
  const { cwd } = io;
  const args = parseExplain(argv);
  if ("error" in args) {
    warn(io, `stanza: ${args.error}\n${usage}`);
    return 2;
  }

  const target = stdinTarget(args.path, cwd);
  if (target.status === "unsupported" || target.status === "failed") {
    warn(io, `stanza: ${target.error}`);
    return 2;
  }

  let bytes: Uint8Array;
  try {
    bytes = readFileSync(resolve(cwd, args.path));
  } catch (error: unknown) {
    const code = errorCode(error);
    const failure =
      code === "ENOENT" ? `no such file: ${args.path}` : `cannot read ${args.path}: ${code}`;

    warn(io, `stanza: ${failure}`);
    return 2;
  }

  const text = decode(bytes);
  if (typeof text !== "string") {
    warn(io, `stanza: ${args.path}: ${text.message}`);
    return 2;
  }

  const root = realpathSync(cwd);
  const body = withoutMark(text);
  const result = explain({
    language: languageOf(args.path),
    path: realpathSync(resolve(cwd, args.path)),
    text: body,
    line: args.line,
    braces: args.noBraces ? "off" : undefined,
    display: (path) => printedPath(path, root),
  });

  if ("error" in result) {
    warn(io, `stanza: ${result.error}`);
    return 2;
  }

  for (const line of result.lines) io.stdout(`${line}\n`);

  if (target.status === "skip" || isGeneratedHeader(body))
    io.stdout("\nnote: --fix and --check skip this file as generated or excluded\n");

  return result.found ? 0 : 1;
}

function formatInputs(inputs: Input[], run: Run): Formatted {
  const warnings = { unread: new Set<string>(), unreadWidth: new Set<string>() };
  const outcomes = inputs.map((input) =>
    formatFile(input, run, run.display?.(input.path) ?? printedPath(input.path, run.cwd), warnings),
  );

  return mergeOutcomes(outcomes, warnings);
}

function status(
  errors: readonly string[],
  formatted: { failed: boolean; findings: Finding[] },
): number {
  if (errors.length > 0 || formatted.failed) return 2;
  return formatted.findings.length > 0 ? 1 : 0;
}

function runFiles(args: Arguments, io: Io): number {
  const { cwd } = io;

  let changed: ReturnType<typeof collectChanged> | undefined;
  let collected: ReturnType<typeof collectFiles>;
  let reason: EmptyReason | undefined;
  try {
    changed = args.changed ? collectChanged(cwd, locate(cwd), args.hunks) : undefined;
    collected = changed ?? collectFiles(args.paths, cwd);
    if (args.paths.length > 0 && collected.files.length === 0 && collected.errors.length === 0)
      reason = emptyReason(args.paths, cwd);
  } catch (error: unknown) {
    warn(
      io,
      `stanza: file selection failed: ${error instanceof Error ? error.message : String(error)}`,
    );

    return 2;
  }

  for (const warning of collected.warnings) warn(io, `stanza: ${warning}`);

  if (reason !== undefined) {
    const paths = args.paths.join(", ");
    const messages: Record<EmptyReason, string> = {
      generated: `stanza: nothing to format in ${paths}, generated files are skipped`,
      ignored: `stanza: nothing to format in ${paths}, git ignores its files`,
      skipped: `stanza: nothing to format in ${paths}, its files are always skipped`,
      none: `stanza: no TypeScript or JavaScript files under ${paths}`,
    };

    warn(io, messages[reason]);
  }

  const inputs = collected.files.map((path) => ({
    path,
    output: printedPath(path, cwd),
    changedLines: args.hunks ? changed?.changedLines?.get(path) : undefined,
  }));

  const run = { cwd, mode: args.mode, braces: args.braces, write: true };
  const { workerCount, formatPooled } = require("./pool.ts") as typeof import("./pool.ts");
  const count = workerCount(inputs.length, io.env);
  const pooled = count > 0 ? formatPooled(inputs, run, count) : undefined;
  const result =
    pooled?.formatted ??
    formatInputs(
      inputs.map((input) => ({ ...input, read: () => readText(input.path) })),
      run,
    );

  for (const error of pooled?.errors ?? []) warn(io, `stanza: ${error}`);

  printFindings(result.findings, args.json, (line) => io.stdout(`${line}\n`));

  for (const error of collected.errors) warn(io, `stanza: ${error}`);

  warnUnread(io, cwd, result.unread, result.unreadWidth);
  return status([...collected.errors, ...(pooled?.errors ?? [])], result);
}

function shellWord(word: string): string {
  return /^[\w@%+:,./-]+$/.test(word) ? word : `'${word.replaceAll("'", "'\\''")}'`;
}

function repairLines(
  findings: Finding[],
  files: StagedFile[],
  cwd: string,
  braces: Braces | undefined,
): string[] {
  const fixable = new Set(
    findings.filter((finding) => finding.fixable).map((finding) => finding.path),
  );

  const targets = files.filter((file) => fixable.has(printedPath(file.path, cwd)));
  const scoped = targets.filter((file) => file.lines !== undefined);
  const whole = targets.filter((file) => file.lines === undefined);
  const dirty = whole.filter((file) => file.unstaged);
  const clean = whole.filter((file) => !file.unstaged);
  const names = (list: StagedFile[]) =>
    list.map((file) => shellWord(printedPath(file.path, cwd))).join(" ");

  const lines: string[] = [];
  if (scoped.length > 0)
    lines.push(
      `fix the staged lines of these by hand and restage them with git add -p, --fix would change the whole file: ${names(scoped)}`,
    );

  if (dirty.length > 0)
    lines.push(
      `these also have unstaged changes, fix them with --fix and restage by hand: ${names(dirty)}`,
    );

  if (clean.length > 0) {
    const flags = braces === undefined ? "" : { on: " --braces", off: " --no-braces" }[braces];

    lines.push(
      `fix and restage with: cd ${shellWord(cwd)} && stanza --fix${flags} -- ${names(clean)} && git --literal-pathspecs add -- ${names(clean)}`,
    );
  }

  return lines;
}

function runStaged(args: Arguments, io: Io): number {
  const collected = collectStaged(io.cwd, args.hunks);
  const files = collected.ok ? collected.files : [];
  const result = formatInputs(
    files.map((file) => ({
      path: file.path,
      read: () => decode(file.bytes),
      changedLines: file.lines,
    })),
    { cwd: io.cwd, mode: args.mode, braces: args.braces, write: false },
  );

  printFindings(result.findings, args.json, (line) => io.stdout(`${line}\n`));

  if (!collected.ok) warn(io, `stanza: ${collected.error}`);
  else for (const line of repairLines(result.findings, files, io.cwd, args.braces)) warn(io, line);

  warnUnread(io, io.cwd, result.unread, result.unreadWidth);
  return status(collected.ok ? [] : [collected.error], result);
}

function warn(io: Io, line: string): void {
  io.stderr(`${line}\n`);
}

function warnUnread(io: Io, cwd: string, unread: Set<string>, unreadWidth: Set<string>): void {
  for (const [fileset, message] of [
    [
      unread,
      (files: string) =>
        `stanza: could not tell whether ${files} enforces braces, so braces stay; pass --braces or --no-braces to settle it`,
    ],
    [
      unreadWidth,
      (files: string) =>
        `stanza: could not read the line width from ${files}, so stanza used the next source for it`,
    ],
  ] as const) {
    if (fileset.size === 0) continue;

    const files = [...fileset]
      .map((path) => printedPath(path, cwd))
      .sort()
      .join(", ");

    warn(io, message(files));
  }
}

function runWriteHook(
  cwdInput: string,
  toolInput: { file_path: string; content: string } & Record<string, unknown>,
  args: string[],
  io: Io,
): number {
  let cwd: string;
  try {
    cwd = realpathSync(cwdInput);
  } catch {
    return 0;
  }

  let target: ReturnType<typeof stdinTarget>;
  try {
    target = stdinTarget(toolInput.file_path, cwd);
  } catch {
    return 0;
  }

  if (target.status === "failed") {
    warn(io, `stanza hook: ${target.error}`);
    return 1;
  }

  if (target.status !== "format" || target.root === undefined) return 0;
  if (!landsWithin(target.root, target.path) || ignoredByGit(target.root, target.path)) return 0;
  if (args.includes("--hunks") && trackedInHead(target.root, target.path)) return 0;

  const result = formatInputs([{ path: target.path, read: () => toolInput.content }], {
    cwd,
    mode: "fix",
    braces: bracesFlag((flag) => args.includes(flag)),
    write: false,
  });

  const fixed = result.kept.get(target.path);
  if (fixed !== undefined)
    io.stdout(
      `${JSON.stringify({
        hookSpecificOutput: {
          hookEventName: "PreToolUse",
          updatedInput: { ...toolInput, content: fixed },
          additionalContext: `stanza formatted ${toolInput.file_path} before writing it, so read it again before editing.`,
        },
      })}\n`,
    );

  warnUnread(io, cwd, result.unread, result.unreadWidth);
  return 0;
}

interface Repository {
  location: Location;
  written: Set<string> | undefined;
}

function repositories(cwd: string, written: Set<string> | undefined): Repository[] {
  if (written === undefined) {
    const location = locate(cwd);
    return location.kind === "outside" ? [] : [{ location, written }];
  }

  const found = new Map<string, Repository & { written: Set<string> }>();
  for (const path of written) {
    let real: string;
    try {
      real = realpathSync(path);
    } catch {
      continue;
    }

    const location = locate(dirname(path));
    if (location.kind === "outside") continue;
    if (location.kind === "repository" && !landsWithin(location.root, real)) continue;

    const key = location.kind === "repository" ? location.root : location.error;
    const repository = found.get(key) ?? { location, written: new Set<string>() };
    repository.written.add(real);
    found.set(key, repository);
  }

  return [...found.values()];
}

function fixChanged(
  cwdInput: string,
  written: Set<string> | undefined,
  args: string[],
  io: Io,
): Formatted | number {
  let cwd: string;
  try {
    cwd = realpathSync(cwdInput);
  } catch {
    return 0;
  }

  const hunks = args.includes("--hunks");
  const files: string[] = [];
  const changedLines = new Map<string, Changed>();

  let failed = false;
  for (const { location, written: kept } of repositories(cwd, written)) {
    const collected = collectChanged(cwd, location, hunks, kept);
    for (const warning of collected.warnings) warn(io, `stanza hook: ${warning}`);
    for (const error of collected.errors) warn(io, `stanza hook: ${error}`);

    failed ||= collected.errors.length > 0;
    files.push(...collected.files);
    for (const [path, lines] of collected.changedLines) changedLines.set(path, lines);
  }

  if (failed) return 1;

  const result = formatInputs(
    files.map((path) => ({
      path,
      read: () => readText(path),
      changedLines: hunks ? changedLines.get(path) : undefined,
    })),
    {
      cwd,
      mode: "fix",
      braces: bracesFlag((flag) => args.includes(flag)),
      write: true,
      display: (path) => (landsWithin(cwd, path) ? printedPath(path, cwd) : path),
    },
  );

  warnUnread(io, cwd, result.unread, result.unreadWidth);
  return result;
}

function runStopHook(
  input: { cwd: string; written: Set<string> | undefined },
  args: string[],
  io: Io,
): number {
  const result = fixChanged(input.cwd, input.written, args, io);
  if (typeof result === "number") return result;

  const reason = blockReason(result.findings, result.rewritten);
  if (reason !== undefined) io.stdout(`${JSON.stringify({ decision: "block", reason })}\n`);

  return 0;
}

function runEditHook(input: { cwd: string; paths: string[] }, args: string[], io: Io): number {
  const result = fixChanged(input.cwd, new Set(input.paths), args, io);
  if (typeof result === "number") return result;

  if (result.rewritten.length > 0)
    io.stdout(
      `${JSON.stringify({
        hookSpecificOutput: {
          hookEventName: "PostToolUse",
          additionalContext: rereadLine("formatted", result.rewritten),
        },
      })}\n`,
    );

  return 0;
}

function runHook(args: string[], io: Io): number {
  const call = hookCall(args, io);
  return typeof call === "number" ? call : runHookCall(call, args, io);
}

export function runHookCall(call: HookCall, args: string[], io: Io): number {
  if (call.event === "write") return runWriteHook(call.cwd, call.toolInput, args, io);
  if (call.event === "edit") return runEditHook(call, args, io);
  return runStopHook(call, args, io);
}

export function main(argv: string[], io: Io): number {
  beginInvocation();

  if (argv[0] === "hook") return runHook(argv.slice(1), io);
  if (argv[0] === "explain") return runExplain(argv.slice(1), io);

  const [only] = argv;
  if (argv.length === 1 && (only === "--help" || only === "-h")) {
    io.stdout(`${help()}\n`);
    return 0;
  }

  if (argv.length === 1 && only === "--version") {
    const commit = typeof STANZA_COMMIT === "string" ? STANZA_COMMIT : undefined;
    io.stdout(`${commit === undefined ? `stanza ${version}` : `stanza ${version} (${commit})`}\n`);
    return 0;
  }

  const args = parseArguments(argv);
  if ("error" in args) {
    warn(io, `stanza: ${args.error}\n${usage}`);
    return 2;
  }

  if (args.stdin !== undefined) return runStdin(args.stdin, args, io);
  if (args.staged) return runStaged(args, io);

  return runFiles(args, io);
}

if (import.meta.main) process.exitCode = main(process.argv.slice(2), systemIo());
