#!/usr/bin/env bun
import { readFileSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, extname, relative } from "node:path";
import { bracesEnforced } from "./config.ts";
import {
  collectChanged,
  collectFiles,
  collectStaged,
  isGeneratedHeader,
  locate,
  stdinTarget,
  type StagedFile,
} from "./files.ts";
import { blockReason, hookInput } from "./hook.ts";
import { processFile } from "./index.ts";
import type { Finding, Mode } from "./types.ts";

interface Arguments {
  changed: boolean;
  json: boolean;
  mode: Mode;
  noBraces: boolean;
  paths: string[];
  staged: boolean;
  stdin: string | undefined;
}

interface Context {
  args: Arguments;
  cwd: string;
}

interface TextResult {
  findings: Finding[];
  fixed: string | undefined;
  parseError: boolean;
}

const usage =
  "Usage: stanza (--fix | --check) [--changed | --stdin <path> | [--] <paths...>] [--json] [--no-braces]\n       stanza --check --staged [--json] [--no-braces]\n       stanza hook [--no-braces]";

const switches = new Set(["--changed", "--json", "--no-braces", "--staged"]);

function parseArguments(args: string[]): Arguments | undefined {
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
      if (mode !== undefined) return undefined;
      mode = arg.slice(2) as Mode;
      continue;
    }

    if (switches.has(arg)) {
      if (seen.has(arg)) return undefined;
      seen.add(arg);
      continue;
    }

    if (arg === "--stdin") {
      const path = queue.next().value;
      if (stdin !== undefined || path === undefined || path.startsWith("-")) return undefined;

      stdin = path;
      continue;
    }

    if (arg.startsWith("-")) return undefined;

    paths.push(arg);
  }

  const changed = seen.has("--changed");
  const staged = seen.has("--staged");
  const sources = [changed, staged, paths.length > 0, stdin !== undefined].filter(Boolean).length;
  if (mode === undefined || sources !== 1 || (staged && mode === "fix")) return undefined;

  return {
    changed,
    json: seen.has("--json"),
    mode,
    noBraces: seen.has("--no-braces"),
    paths,
    staged,
    stdin,
  };
}

function printedPath(path: string, cwd: string): string {
  const output = relative(cwd, path);
  return output || path;
}

function compareFindings(left: Finding, right: Finding): number {
  return left.path.localeCompare(right.path) || left.line - right.line || left.col - right.col;
}

function printFindings(findings: Finding[], json: boolean, print: (line: string) => void): void {
  if (json) {
    print(JSON.stringify(findings));
    return;
  }

  for (const finding of findings)
    print(`${finding.path}:${finding.line}:${finding.col} ${finding.rule} ${finding.message}`);
}

const utf8 = new TextDecoder("utf-8", { fatal: true });

function decode(bytes: Uint8Array): string | { message: string } {
  try {
    return utf8.decode(bytes);
  } catch {
    return { message: "not valid UTF-8, left untouched" };
  }
}

function readText(path: string): string | { message: string } {
  try {
    return decode(readFileSync(path));
  } catch (error: unknown) {
    return { message: String(error) };
  }
}

function processText(
  path: string,
  text: string | { message: string },
  context: Context,
): TextResult {
  const output = printedPath(path, context.cwd);
  if (typeof text !== "string")
    return {
      findings: [
        { path: output, line: 1, col: 1, rule: "parse", message: text.message, fixable: false },
      ],
      fixed: undefined,
      parseError: true,
    };

  if (isGeneratedHeader(text)) return { findings: [], fixed: undefined, parseError: false };

  const result = processFile(path, text, context.args.mode, {
    keepBraces: context.args.noBraces || bracesEnforced(dirname(path), extname(path)),
  });

  const changed = context.args.mode === "fix" && !result.parseError && result.text !== text;
  return {
    findings: result.findings.map((finding) => ({ ...finding, path: output })),
    fixed: changed ? result.text : undefined,
    parseError: result.parseError,
  };
}

function runStdin(input: string, context: Context): number {
  let source: Uint8Array;
  try {
    source = readFileSync(0);
  } catch (error: unknown) {
    console.error(String(error));
    return 2;
  }

  const target = stdinTarget(input, context.cwd);
  const result =
    target.status === "format"
      ? processText(target.path, decode(source), context)
      : { findings: [], fixed: undefined, parseError: false };

  const fix = context.args.mode === "fix";
  if (fix) process.stdout.write(result.fixed ?? source);

  printFindings(
    result.findings.sort(compareFindings),
    context.args.json,
    fix ? console.error : console.log,
  );

  if (target.status === "unsupported" || target.status === "failed") {
    console.error(target.error);
    return 2;
  }

  if (result.parseError) return 2;
  return result.findings.length > 0 ? 1 : 0;
}

function writeFixed(path: string, text: string, output: string): Finding | undefined {
  try {
    writeFileSync(path, text, "utf8");
  } catch (error: unknown) {
    return { path: output, line: 1, col: 1, rule: "write", message: String(error), fixable: false };
  }
}

function formatFiles(
  files: string[],
  context: Context,
): { findings: Finding[]; failed: boolean; rewritten: string[] } {
  const findings: Finding[] = [];
  const rewritten: string[] = [];

  let failed = false;
  for (const path of files) {
    const result = processText(path, readText(path), context);
    findings.push(...result.findings);
    failed ||= result.parseError;

    if (result.fixed === undefined) continue;

    const output = printedPath(path, context.cwd);
    const unwritten = writeFixed(path, result.fixed, output);
    if (unwritten) {
      findings.push(unwritten);
      failed = true;
      continue;
    }

    rewritten.push(output);
  }

  return { findings: findings.sort(compareFindings), failed, rewritten };
}

function runFiles(context: Context): number {
  const { args, cwd } = context;
  const collected = args.changed ? collectChanged(cwd) : collectFiles(args.paths, cwd);
  for (const warning of collected.warnings) console.error(warning);

  const { findings, failed } = formatFiles(collected.files, context);
  printFindings(findings, args.json, console.log);

  if (collected.errors.length > 0) {
    for (const error of collected.errors) console.error(error);
    return 2;
  }

  if (failed) return 2;
  return findings.length > 0 ? 1 : 0;
}

function shellWord(word: string): string {
  return /^[\w@%+:,./-]+$/.test(word) ? word : `'${word.replaceAll("'", "'\\''")}'`;
}

function repairLines(findings: Finding[], files: StagedFile[], context: Context): string[] {
  const fixable = new Set(
    findings.filter((finding) => finding.fixable).map((finding) => finding.path),
  );

  const targets = files.filter((file) => fixable.has(printedPath(file.path, context.cwd)));
  const dirty = targets.filter((file) => file.unstaged);
  const clean = targets.filter((file) => !file.unstaged);
  const names = (list: StagedFile[]) =>
    list.map((file) => shellWord(printedPath(file.path, context.cwd))).join(" ");

  const lines: string[] = [];
  if (dirty.length > 0)
    lines.push(
      `these also have unstaged changes, fix them with --fix and restage by hand: ${names(dirty)}`,
    );

  if (clean.length > 0) {
    const flags = context.args.noBraces ? " --no-braces" : "";
    lines.push(
      `fix and restage with: cd ${shellWord(context.cwd)} && stanza --fix${flags} -- ${names(clean)} && git --literal-pathspecs add -- ${names(clean)}`,
    );
  }

  return lines;
}

function runStaged(context: Context): number {
  const collected = collectStaged(context.cwd);
  const files = collected.ok ? collected.files : [];
  const findings: Finding[] = [];

  let failed = false;
  for (const file of files) {
    const result = processText(file.path, decode(file.bytes), context);
    findings.push(...result.findings);
    failed ||= result.parseError;
  }

  findings.sort(compareFindings);
  printFindings(findings, context.args.json, console.log);

  if (!collected.ok) {
    console.error(collected.error);
    return 2;
  }

  for (const line of repairLines(findings, files, context)) console.error(line);

  if (failed) return 2;
  return findings.length > 0 ? 1 : 0;
}

function warn(line: string): void {
  process.stderr.write(`${line}\n`);
}

function runHook(args: string[]): number {
  if (process.env.AGENT_HOOKS === "0") return 0;

  const unexpected = args.find((arg, index) => arg !== "--no-braces" || index > 0);
  if (unexpected !== undefined) {
    warn(`stanza hook: unexpected argument ${unexpected}\n${usage}`);
    return 1;
  }

  const input = hookInput(readFileSync(0, "utf8"), process.cwd());
  if ("error" in input) {
    warn(`stanza hook: ${input.error}`);
    return 1;
  }

  if (input.stopHookActive) return 0;

  let cwd: string;
  try {
    cwd = realpathSync(input.cwd);
  } catch {
    return 0;
  }

  const location = locate(cwd);
  if (location.kind === "outside") return 0;

  const collected = collectChanged(cwd, location);
  for (const warning of collected.warnings) warn(warning);

  if (collected.errors.length > 0) {
    for (const error of collected.errors) warn(error);
    return 1;
  }

  const context = {
    args: {
      changed: true,
      json: false,
      mode: "fix" as const,
      noBraces: args.includes("--no-braces"),
      paths: [],
      staged: false,
      stdin: undefined,
    },
    cwd,
  };

  const result = formatFiles(collected.files, context);
  const reason = blockReason(result.findings, result.rewritten);
  if (reason !== undefined) console.log(JSON.stringify({ decision: "block", reason }));

  return 0;
}

function run(): number {
  const argv = process.argv.slice(2);
  if (argv[0] === "hook") return runHook(argv.slice(1));

  const args = parseArguments(argv);
  if (!args) {
    console.error(usage);
    return 2;
  }

  const context = { args, cwd: process.cwd() };
  if (args.stdin !== undefined) return runStdin(args.stdin, context);
  if (args.staged) return runStaged(context);

  return runFiles(context);
}

process.exitCode = run();
