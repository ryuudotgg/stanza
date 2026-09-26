#!/usr/bin/env bun
import { readFileSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, extname, isAbsolute, join, relative, sep } from "node:path";
import { bracesEnforced } from "./config.ts";
import { collectChanged, collectFiles, isGeneratedHeader, locate, stdinTarget } from "./files.ts";
import { blockReason, hookInput } from "./hook.ts";
import { processFile } from "./index.ts";
import type { Finding, Mode } from "./types.ts";

interface Arguments {
  changed: boolean;
  json: boolean;
  mode: Mode;
  noBraces: boolean;
  paths: string[];
  stdin: string | undefined;
}

interface Context {
  args: Arguments;
  cwd: string;
  configCwd: string;
  configRoot: string | undefined;
}

interface TextResult {
  findings: Finding[];
  fixed: string | undefined;
  parseError: boolean;
}

const usage =
  "Usage: stanza (--fix | --check) [--changed | --stdin <path> | <paths...>] [--json] [--no-braces]\n       stanza hook [--no-braces]";

function parseArguments(args: string[]): Arguments | undefined {
  const paths: string[] = [];
  const queue = args.values();

  let changed = false;
  let json = false;
  let noBraces = false;
  let mode: Mode | undefined;
  let stdin: string | undefined;
  for (const arg of queue) {
    if (arg === "--fix" || arg === "--check") {
      if (mode !== undefined) return undefined;
      mode = arg.slice(2) as Mode;
      continue;
    }

    if (arg === "--changed") {
      if (changed) return undefined;
      changed = true;
      continue;
    }

    if (arg === "--json") {
      if (json) return undefined;
      json = true;
      continue;
    }

    if (arg === "--no-braces") {
      if (noBraces) return undefined;
      noBraces = true;
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

  const sources = [changed, paths.length > 0, stdin !== undefined].filter(Boolean).length;
  if (mode === undefined || sources !== 1) return undefined;

  return { changed, json, mode, noBraces, paths, stdin };
}

function printedPath(path: string, cwd: string): string {
  const output = relative(cwd, path);
  return output || path;
}

function configDirectory(path: string, cwd: string, root: string | undefined): string {
  const directory = dirname(path);
  if (!root) return directory;

  const relativeDirectory = relative(cwd, directory);
  const outside =
    relativeDirectory === ".." ||
    relativeDirectory.startsWith(`..${sep}`) ||
    isAbsolute(relativeDirectory);

  if (outside) return directory;

  return join(root, relativeDirectory);
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
    keepBraces:
      context.args.noBraces ||
      bracesEnforced(configDirectory(path, context.configCwd, context.configRoot), extname(path)),
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

function buildContext(args: Arguments, cwd: string): Context {
  const configRoot = process.env.STANZA_CONFIG_ROOT;
  return { args, cwd, configCwd: configRoot ? realpathSync(cwd) : cwd, configRoot };
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

  const context = buildContext(
    {
      changed: true,
      json: false,
      mode: "fix",
      noBraces: args.includes("--no-braces"),
      paths: [],
      stdin: undefined,
    },
    cwd,
  );

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

  const context = buildContext(args, process.cwd());
  if (args.stdin !== undefined) return runStdin(args.stdin, context);

  return runFiles(context);
}

process.exitCode = run();
