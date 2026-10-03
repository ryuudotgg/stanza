import { randomUUID } from "node:crypto";
import {
  accessSync,
  chmodSync,
  constants,
  readFileSync,
  realpathSync,
  renameSync,
  type Stats,
  statSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { dirname, join } from "node:path";
import { errorCode } from "./files.ts";
import type { Changed } from "./engine/model.ts";
import { compareFindings, type Braces, type Finding, type Mode } from "./engine/types.ts";
import { decode, type Decoded, formatText } from "./step.ts";

export interface Input {
  path: string;
  read: () => Decoded;
  changedLines?: Changed | undefined;
}

export interface FormatRun {
  mode: Mode;
  braces: Braces | undefined;
  write: boolean;
}

export interface Warnings {
  unread: Set<string>;
  unreadWidth: Set<string>;
}

export interface FileOutcome {
  path: string;
  findings: Finding[];
  failed: boolean;
  rewritten?: string;
  kept?: string;
}

export interface Formatted extends Warnings {
  findings: Finding[];
  failed: boolean;
  rewritten: string[];
  kept: Map<string, string>;
}

export function readText(path: string): Decoded {
  try {
    return decode(readFileSync(path));
  } catch (error: unknown) {
    return { message: String(error) };
  }
}

function removeQuietly(path: string): void {
  try {
    unlinkSync(path);
  } catch {}
}

function replaceFile(real: string, text: string, target: Stats): boolean {
  const temp = join(dirname(real), `.stanza-${randomUUID()}.tmp`);
  try {
    writeFileSync(temp, text, { encoding: "utf8", flag: "wx", mode: 0o600 });
  } catch (error: unknown) {
    removeQuietly(temp);
    if (["EACCES", "EPERM"].includes(errorCode(error))) return false;
    throw error;
  }

  try {
    const created = statSync(temp);
    if (created.uid !== target.uid || created.gid !== target.gid) {
      removeQuietly(temp);
      return false;
    }

    chmodSync(temp, target.mode & 0o7777);
    renameSync(temp, real);
    return true;
  } catch (error: unknown) {
    removeQuietly(temp);
    throw error;
  }
}

function writeInPlace(real: string, text: string): void {
  const original = readFileSync(real);
  try {
    writeFileSync(real, text, "utf8");
  } catch (error: unknown) {
    try {
      writeFileSync(real, original);
    } catch {}

    throw error;
  }
}

function writeFixed(path: string, text: string, output: string): Finding | undefined {
  try {
    const real = realpathSync(path);
    const target = statSync(real);
    accessSync(real, constants.W_OK);

    if (target.nlink > 1 || !replaceFile(real, text, target)) writeInPlace(real, text);
  } catch (error: unknown) {
    return { path: output, line: 1, col: 1, rule: "write", message: String(error), fixable: false };
  }
}

export function formatFile(
  input: Input,
  run: FormatRun,
  output: string,
  warnings: Warnings,
): FileOutcome {
  const result = formatText(input.path, input.read(), {
    mode: run.mode,
    braces: run.braces,
    changedLines: input.changedLines,
    ...warnings,
  });

  const outcome: FileOutcome = {
    path: input.path,
    findings: result.findings.map((finding) => ({ ...finding, path: output })),
    failed: result.parseError,
  };

  if (result.fixed === undefined) return outcome;
  if (!run.write) return { ...outcome, kept: result.fixed };

  const unwritten = writeFixed(input.path, result.fixed, output);
  if (unwritten) return { ...outcome, findings: [...outcome.findings, unwritten], failed: true };

  return { ...outcome, rewritten: output };
}

export function mergeOutcomes(outcomes: FileOutcome[], warnings: Warnings): Formatted {
  const findings: Finding[] = [];
  const rewritten: string[] = [];
  const kept = new Map<string, string>();

  let failed = false;
  for (const outcome of outcomes) {
    findings.push(...outcome.findings);
    failed ||= outcome.failed;
    if (outcome.rewritten !== undefined) rewritten.push(outcome.rewritten);
    if (outcome.kept !== undefined) kept.set(outcome.path, outcome.kept);
  }

  return { findings: findings.sort(compareFindings), failed, rewritten, kept, ...warnings };
}
