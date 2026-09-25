#!/usr/bin/env bun
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, extname, relative } from "node:path";
import { bracesEnforced } from "./config.ts";
import { collectChanged, collectFiles, isGeneratedHeader } from "./files.ts";
import { processFile } from "./index.ts";
import type { Finding, Mode } from "./types.ts";

interface Arguments {
  changed: boolean;
  json: boolean;
  mode: Mode;
  noBraces: boolean;
  paths: string[];
}

const usage = "Usage: stanza (--fix | --check) [--changed | <paths...>] [--json] [--no-braces]";

function parseArguments(args: string[]): Arguments | undefined {
  const paths: string[] = [];

  let changed = false;
  let json = false;
  let noBraces = false;
  let mode: Mode | undefined;
  for (const arg of args) {
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

    if (arg.startsWith("-")) return undefined;

    paths.push(arg);
  }

  if (mode === undefined || (changed && paths.length > 0) || (!changed && paths.length === 0))
    return undefined;

  return { changed, json, mode, noBraces, paths };
}

function printedPath(path: string, cwd: string): string {
  const output = relative(cwd, path);
  return output || path;
}

function compareFindings(left: Finding, right: Finding): number {
  return left.path.localeCompare(right.path) || left.line - right.line || left.col - right.col;
}

function printFindings(findings: Finding[], json: boolean): void {
  if (json) {
    console.log(JSON.stringify(findings));
    return;
  }

  for (const finding of findings)
    console.log(
      `${finding.path}:${finding.line}:${finding.col} ${finding.rule} ${finding.message}`,
    );
}

const utf8 = new TextDecoder("utf-8", { fatal: true });

function readText(path: string): string | { message: string } {
  try {
    return utf8.decode(readFileSync(path));
  } catch (error: unknown) {
    const message = error instanceof TypeError ? "not valid UTF-8, left untouched" : String(error);
    return { message };
  }
}

function run(): number {
  const args = parseArguments(process.argv.slice(2));
  if (!args) {
    console.error(usage);
    return 2;
  }

  const cwd = process.cwd();
  const collected = args.changed ? collectChanged(cwd) : collectFiles(args.paths, cwd);
  for (const warning of collected.warnings) console.error(warning);

  const findings: Finding[] = [];

  let parseError = false;
  for (const path of collected.files) {
    const output = printedPath(path, cwd);
    const text = readText(path);
    if (typeof text !== "string") {
      findings.push({
        path: output,
        line: 1,
        col: 1,
        rule: "parse",
        message: text.message,
        fixable: false,
      });

      parseError = true;
      continue;
    }

    if (isGeneratedHeader(text)) continue;

    const result = processFile(path, text, args.mode, {
      keepBraces: args.noBraces || bracesEnforced(dirname(path), extname(path)),
    });

    if (args.mode === "fix" && !result.parseError && result.text !== text)
      writeFileSync(path, result.text, "utf8");

    findings.push(...result.findings.map((finding) => ({ ...finding, path: output })));
    parseError ||= result.parseError;
  }

  findings.sort(compareFindings);
  printFindings(findings, args.json);

  if (collected.errors.length > 0) {
    for (const error of collected.errors) console.error(error);
    return 2;
  }

  if (parseError) return 2;
  return findings.length > 0 ? 1 : 0;
}

process.exitCode = run();
