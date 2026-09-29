import { createHash } from "node:crypto";
import { readFileSync, realpathSync, writeFileSync } from "node:fs";
import { basename, dirname, extname, join, relative, resolve, sep } from "node:path";
import type { Comment } from "../src/engine/model.ts";
import { languageOf } from "../src/languages/index.ts";
import type { OracleSide, OracleStatement } from "../src/languages/language.ts";
import { document, lineAt } from "../src/engine/doc.ts";
import { explainer } from "../src/engine/explain.ts";
import { RULES, type RuleId } from "../src/engine/rules.ts";
import { collectFiles, isGeneratedHeader } from "../src/files.ts";
import { decode, fixText, keepBraces, withoutMark } from "../src/step.ts";
import type { FileResult, Finding, Options } from "../src/engine/types.ts";

export const INVARIANTS = [
  "idempotence",
  "preservation",
  "program shape",
  "fixable left",
  "agreement",
  "directives",
  "full hunk",
  "empty hunk",
  "crash",
] as const;

export type Invariant = (typeof INVARIANTS)[number];

export type Fix = typeof fixText;

export type Verdict =
  | { kind: "parse failure" }
  | {
      kind: "judged";
      output: string | undefined;
      findings: string | undefined;
      explain: string | undefined;
      broken: Invariant[];
    };

export interface Side {
  text: string;
  oracle: OracleSide;
}

export function side(path: string, text: string): Side {
  const body = withoutMark(text);
  return { text: body, oracle: languageOf(path).oracle().side(path, body) };
}

const SEAM = "\0";

function seamedLines(side: Side): string[] {
  const offsets = side.oracle.blocks
    .flatMap((block) => [block.start, block.end - 1])
    .sort((left, right) => left - right);

  const seamed = [-1, ...offsets]
    .map((offset, index) => side.text.slice(offset + 1, offsets[index] ?? side.text.length))
    .join(SEAM);

  return seamed.split("\n").filter((line) => line.replaceAll(SEAM, "").trim() !== "");
}

const HORIZONTAL = /[^\S\r\n]/;

function matchesAcrossSeams(pattern: string, line: string): boolean {
  if (!pattern.includes(SEAM)) return pattern === line;

  const parts = pattern.split(SEAM);
  const target = line.replaceAll(SEAM, "");

  let cursor = 0;
  for (const [index, part] of parts.entries()) {
    const start = index > 0 ? part.replace(/^[^\S\r\n]+/, "") : part;
    const literal = index < parts.length - 1 ? start.replace(/[^\S\r\n]+$/, "") : start;
    if (index > 0) while (cursor < target.length && HORIZONTAL.test(target[cursor]!)) cursor++;
    if (!target.startsWith(literal, cursor)) return false;

    cursor += literal.length;
  }

  return cursor === target.length;
}

function sameLines(original: string[], fixed: string[]): boolean {
  return (
    original.length === fixed.length &&
    original.every(
      (line, index) =>
        matchesAcrossSeams(line, fixed[index]!) || matchesAcrossSeams(fixed[index]!, line),
    )
  );
}

function foreignBlankLines(text: string): number {
  const lines = text.split("\n");

  let foreign = 0;
  for (let index = 1; index < lines.length - 1; index++) {
    const line = lines[index]!;
    if (line.trim() === "" && line.endsWith("\r") !== lines[index - 1]!.endsWith("\r")) foreign++;
  }

  return foreign;
}

function commentBytes(side: Side): string[] {
  return side.oracle.comments.map((comment) => side.text.slice(comment.start, comment.end));
}

function sameItems(left: string[], right: string[]): boolean {
  return left.length === right.length && left.every((item, index) => item === right[index]);
}

export function isIdempotent(
  path: string,
  fixed: string,
  options: Options,
  fix: Fix = fixText,
): boolean {
  return fix(path, fixed, "fix", options).text === fixed;
}

export function preservesText(original: Side, fixed: Side): boolean {
  return (
    original.text.endsWith("\n") === fixed.text.endsWith("\n") &&
    foreignBlankLines(fixed.text) <= foreignBlankLines(original.text) &&
    sameItems(commentBytes(original), commentBytes(fixed)) &&
    sameLines(seamedLines(original), seamedLines(fixed))
  );
}

export function preservesShape(original: Side, fixed: Side): boolean {
  const fixedShape = fixed.oracle.shape();
  return fixedShape !== undefined && fixedShape === original.oracle.shape();
}

export function leavesNothingFixable(
  path: string,
  fixed: string,
  options: Options,
  fix: Fix = fixText,
): boolean {
  return !fix(path, fixed, "check", options).findings.some((finding) => finding.fixable);
}

interface ContentLine {
  line: number;
  blanks: number;
  key: string;
  braceOnly: boolean;
  comment: boolean;
}

function contentLines(text: string, commentLines: ReadonlySet<number>): ContentLine[] {
  const lines: ContentLine[] = [];

  let blanks = 0;
  for (const [index, raw] of text.split("\n").entries()) {
    if (raw.trim() === "") {
      blanks++;
      continue;
    }

    const code = raw.replace(/\s/g, "");
    const key = code.replace(/[{}]/g, "");
    const braceOnly = key === "";

    lines.push({
      line: index + 1,
      blanks,
      key: braceOnly ? code : key,
      braceOnly,
      comment: commentLines.has(index + 1),
    });

    blanks = 0;
  }

  return lines;
}

function aligned(before: ContentLine[], after: ContentLine[]): [ContentLine, ContentLine][] {
  const pairs: [ContentLine, ContentLine][] = [];

  let next = 0;
  let carried = 0;
  for (const line of before) {
    const other = after[next];
    if (other?.key === line.key) {
      pairs.push([{ ...line, blanks: line.blanks + carried }, other]);
      carried = 0;
      next++;
      continue;
    }

    if (!line.braceOnly) return [];

    carried += line.blanks;
  }

  return next === after.length ? pairs : [];
}

function blankDirection(finding: Finding): number {
  if (finding.rule === "edge-blank") return -1;
  if (!(finding.rule in RULES)) return 0;

  const gap = RULES[finding.rule as RuleId].gap;
  return gap === "none" ? -1 : gap === "at-least-one" ? 1 : 0;
}

function commentLines(side: Side): Set<number> {
  const doc = document("", side.text, side.oracle.comments);
  const lines = new Set<number>();
  for (const comment of side.oracle.comments) {
    const first = lineAt(doc, comment.start);
    if (doc.lines[first - 1]!.slice(0, comment.start - doc.lineStarts[first - 1]!).trim() !== "")
      continue;
    for (let line = first; line <= lineAt(doc, comment.end - 1); line++) lines.add(line);
  }

  return lines;
}

function spacingAgrees(original: Side, fixed: Side, findings: Finding[]): boolean {
  const pairs = aligned(
    contentLines(original.text, commentLines(original)),
    contentLines(fixed.text, new Set()),
  );

  if (pairs.length === 0 && original.text.trim() !== "") return false;

  const changes = pairs.map(([before, after]) => Math.sign(after.blanks - before.blanks));
  const claimed = new Set<number>();
  for (const finding of findings) {
    const want = blankDirection(finding);
    if (want === 0) continue;

    let at = pairs.findIndex(([before]) => before.line >= finding.line);
    while (at > 0 && changes[at] !== want && pairs[at - 1]![0].comment) at--;
    if (at < 0 || changes[at] !== want) return false;

    claimed.add(at);
  }

  return changes.every((change, index) => change === 0 || claimed.has(index));
}

export function agrees(
  path: string,
  text: string,
  output: string,
  original: Side,
  fixed: Side,
  findings: Finding[],
): boolean {
  if (findings.some((finding) => finding.fixable) !== (output !== text)) return false;
  if (!spacingAgrees(original, fixed, findings)) return false;

  const braces = findings.filter((finding) => finding.rule === "braces");
  if (braces.length !== original.oracle.blocks.length - fixed.oracle.blocks.length) return false;

  const starts = new Set(
    original.oracle.blocks.filter((block) => block.single).map((block) => block.start),
  );

  const { lineStarts } = document(path, original.text, original.oracle.comments);
  return braces.every((finding) => {
    const lineStart = lineStarts[finding.line - 1];
    return lineStart !== undefined && starts.delete(lineStart + finding.col - 1);
  });
}

export function coversEveryLine(
  path: string,
  text: string,
  fixed: string,
  options: Options,
  fix: Fix = fixText,
): boolean {
  const changedLines = {
    lines: new Set(Array.from({ length: text.split("\n").length }, (_, index) => index + 1)),
    deletedAfter: new Set<number>(),
  };

  return fix(path, text, "fix", { ...options, changedLines }).text === fixed;
}

export function touchesNoLine(
  path: string,
  text: string,
  options: Options,
  fix: Fix = fixText,
): boolean {
  return (
    fix(path, text, "fix", {
      ...options,
      changedLines: { lines: new Set(), deletedAfter: new Set() },
    }).text === text
  );
}

type Directive = { kind: "ignore" | "off" | "on"; comment: Comment };
function directives(side: Side): Directive[] {
  return side.oracle.comments.flatMap((comment) => {
    const match = /^stanza-(ignore|off|on)\b/.exec(comment.value.trim());
    return match ? [{ kind: match[1] as Directive["kind"], comment }] : [];
  });
}

function offSpan(side: Side, marks: Directive[], index: number): string {
  const comment = marks[index]!.comment;
  const enclosing = side.oracle.container(comment);

  let depth = 1;
  let end = enclosing.end;
  for (const mark of marks.slice(index + 1)) {
    if (mark.comment.start >= enclosing.end) break;
    if (mark.kind === "ignore") continue;

    const container = side.oracle.container(mark.comment);
    if (container.start !== enclosing.start || container.end !== enclosing.end) continue;

    depth += mark.kind === "off" ? 1 : -1;
    if (depth !== 0) continue;

    end = mark.comment.start;
    break;
  }

  return side.text.slice(comment.start, end);
}

function directlyAbove(side: Side, comment: Comment, start: number): boolean {
  const lineStart = side.text.lastIndexOf("\n", comment.start - 1) + 1;
  return (
    /^[^\S\n]*$/.test(side.text.slice(lineStart, comment.start)) &&
    /^[^\S\n]*\n[^\S\n]*$/.test(side.text.slice(comment.end, start))
  );
}

function statementAt(side: Side, comment: Comment): OracleStatement | undefined {
  let top = comment;
  for (const next of side.oracle.comments) {
    if (next.start <= top.start) continue;
    if (!directlyAbove(side, top, next.start)) break;
    top = next;
  }

  const next = side.oracle.statementAfter(top.end);
  if (!next || !directlyAbove(side, top, next.start)) return undefined;

  return next;
}

function statementGaps(left: OracleStatement, right: OracleStatement): boolean {
  const first = left.gaps();
  const second = right.gaps();
  return first[0] === second[0] && first[1] === second[1];
}

export function keepsDirectives(original: Side, fixed: Side): boolean {
  const before = directives(original);
  const after = directives(fixed);
  if (
    before.length !== after.length ||
    before.some((mark, index) => mark.kind !== after[index]!.kind)
  )
    return false;

  for (let index = 0; index < before.length; index++) {
    const left = before[index]!;
    const right = after[index]!;

    if (left.kind === "off" && offSpan(original, before, index) !== offSpan(fixed, after, index))
      return false;
    if (left.kind !== "ignore") continue;

    const statement = statementAt(original, left.comment);
    if (!statement) continue;

    const counterpart = statementAt(fixed, right.comment);
    if (!counterpart || (!statement.clause && statement.skeleton() !== counterpart.skeleton()))
      return false;
    if (!statementGaps(statement, counterpart)) return false;
  }

  return true;
}

function digest(parts: Iterable<string>): string {
  const hash = createHash("sha256");
  const seen = new Map<string, string>();
  for (const part of parts) {
    let partHash = seen.get(part);
    if (partHash === undefined) {
      partHash = sha256(part);
      seen.set(part, partHash);
    }

    hash.update(partHash);
  }

  return hash.digest("hex");
}

function findingsDigest(findings: Finding[]): string {
  return digest(
    findings
      .map(({ line, col, rule, message, fixable }) =>
        JSON.stringify([line, col, rule, message, fixable]),
      )
      .sort(),
  );
}

function* explanationParts(
  path: string,
  text: string,
  findings: Finding[],
  keepBraces: boolean,
): Generator<string> {
  const lines = [...new Set(findings.map((finding) => finding.line))].sort(
    (left, right) => left - right,
  );

  if (lines.length === 0) return;

  const display = (shown: string) => relative(dirname(path), shown) || basename(path);
  const explained = explainer({
    language: languageOf(path),
    path,
    text: withoutMark(text),
    braces: keepBraces ? undefined : "on",
    display,
  });

  if (typeof explained !== "function") {
    yield explained.error;
    return;
  }

  for (const line of lines) {
    const explanation = explained(line);
    if ("error" in explanation) yield explanation.error;
    else yield* explanation.lines;

    yield "\n";
  }
}

export function judge(
  path: string,
  text: string,
  keepBraces: boolean,
  fix: Fix = fixText,
): Verdict {
  const options: Options = { keepBraces };

  let result: FileResult;
  try {
    result = fix(path, text, "fix", options);
  } catch {
    return {
      kind: "judged",
      output: undefined,
      findings: undefined,
      explain: undefined,
      broken: ["crash"],
    };
  }

  if (result.parseError) return { kind: "parse failure" };

  const output = result.text;

  let sides: { original: Side; fixed: Side };
  let checked: Finding[];
  try {
    sides = { original: side(path, text), fixed: side(path, output) };
    checked = fix(path, text, "check", options).findings;
  } catch {
    return { kind: "judged", output, findings: undefined, explain: undefined, broken: ["crash"] };
  }

  const checks: [Invariant, () => boolean][] = [
    ["idempotence", () => isIdempotent(path, output, options, fix)],
    ["preservation", () => preservesText(sides.original, sides.fixed)],
    ["program shape", () => preservesShape(sides.original, sides.fixed)],
    ["fixable left", () => leavesNothingFixable(path, output, options, fix)],
    ["agreement", () => agrees(path, text, output, sides.original, sides.fixed, checked)],
    ["directives", () => keepsDirectives(sides.original, sides.fixed)],
    ["full hunk", () => coversEveryLine(path, text, output, options, fix)],
    ["empty hunk", () => touchesNoLine(path, text, options, fix)],
  ];

  const broken: Invariant[] = [];

  let crashed = false;
  for (const [invariant, holds] of checks)
    try {
      if (!holds()) broken.push(invariant);
    } catch {
      crashed = true;
    }

  const findings = findingsDigest(checked);

  let explain: string | undefined;
  try {
    explain = digest(explanationParts(path, text, checked, keepBraces));
  } catch {
    crashed = true;
  }

  if (crashed) broken.push("crash");
  return { kind: "judged", output, findings, explain, broken };
}

type Entry = { output: string; findings: string; explain: string };
type HashRecord = { [path: string]: Entry };

interface Arguments {
  dirs: string[];
  braces: boolean;
  includeGenerated: boolean;
  record?: { mode: "snapshot" | "against"; file: string };
}

const usage =
  "Usage: bun scripts/corpus.ts [--snapshot <file> | --against <file>] [--include-generated] [--braces] <dir>...";

function parseArguments(args: string[]): Arguments | undefined {
  const dirs: string[] = [];

  let record: Arguments["record"];
  let braces = false;
  let includeGenerated = false;
  for (let index = 0; index < args.length; index++) {
    const arg = args[index]!;
    if (arg === "--braces") {
      braces = true;
      continue;
    }

    if (arg === "--include-generated") {
      includeGenerated = true;
      continue;
    }

    if (arg === "--snapshot" || arg === "--against") {
      const file = args[++index];
      if (record || file === undefined || file.startsWith("-")) return undefined;

      record = { mode: arg === "--snapshot" ? "snapshot" : "against", file };
      continue;
    }

    if (arg.startsWith("-")) return undefined;

    dirs.push(arg);
  }

  return dirs.length > 0 ? { dirs, record, braces, includeGenerated } : undefined;
}

function sha256(content: string | Uint8Array): string {
  return createHash("sha256").update(content).digest("hex");
}

function readRecord(file: string): HashRecord {
  const raw: unknown = JSON.parse(readFileSync(file, "utf8"));
  const object = typeof raw === "object" && raw !== null && !Array.isArray(raw);
  if (!object) throw new Error(`${file} is not a record of path to hash`);

  const values = Object.values(raw);
  if (values.length > 0 && values.every((value) => typeof value === "string"))
    throw new Error(
      `${file} was written by the earlier record shape that held only fix output hashes; take a new snapshot`,
    );

  const valid = values.every(
    (value) =>
      typeof value === "object" &&
      value !== null &&
      !Array.isArray(value) &&
      Object.keys(value).length === 3 &&
      typeof value.output === "string" &&
      typeof value.findings === "string" &&
      typeof value.explain === "string",
  );

  if (!valid) throw new Error(`${file} is not a record of path to hash`);
  return raw as HashRecord;
}

function sortedRecord(record: HashRecord): HashRecord {
  return Object.fromEntries(
    Object.entries(record).sort(([left], [right]) => (left < right ? -1 : 1)),
  );
}

function difference(path: string, previous: HashRecord, current: HashRecord): string | undefined {
  if (!Object.hasOwn(current, path)) return "only in snapshot";
  if (!Object.hasOwn(previous, path)) return "only in run";

  const fields = (["output", "findings", "explain"] as const).filter(
    (field) => previous[path]![field] !== current[path]![field],
  );

  if (fields.length > 0) return `differs (${fields.join(", ")})`;
}

function printDifferences(previous: HashRecord, current: HashRecord): number {
  const paths = [...new Set([...Object.keys(previous), ...Object.keys(current)])].sort();

  let changed = 0;
  for (const path of paths) {
    const label = difference(path, previous, current);
    if (!label) continue;

    changed++;
    console.log(`${label}: ${path}`);
  }

  console.log(`changed: ${changed}`);
  return changed;
}

function inside(path: string, dir: string): boolean {
  const from = relative(dir, path);
  return from === "" || (!from.startsWith(`..${sep}`) && from !== ".." && !from.startsWith(sep));
}

function shown(path: string, cwd: string): string {
  return inside(path, cwd) ? relative(cwd, path) || path : path;
}

function recordKey(path: string, roots: { input: string; real: string }[]): string {
  const root = roots.find((candidate) => inside(path, candidate.real));
  return root ? join(root.input, relative(root.real, path)) : path;
}

function run(): number {
  const args = parseArguments(process.argv.slice(2));
  if (!args) {
    console.error(usage);
    return 2;
  }

  const cwd = process.cwd();
  const recordFile = args.record && resolve(cwd, args.record.file);

  if (
    args.record?.mode === "snapshot" &&
    args.dirs.some((dir) => inside(recordFile!, resolve(cwd, dir)))
  ) {
    console.error(`${args.record.file} is inside a tree being read`);
    return 2;
  }

  let previous: HashRecord | undefined;
  try {
    if (args.record?.mode === "against") previous = readRecord(recordFile!);
  } catch (error: unknown) {
    console.error(String(error));
    return 2;
  }

  const started = performance.now();
  const collected = collectFiles(args.dirs, cwd, args.includeGenerated);
  for (const message of [...collected.warnings, ...collected.errors]) console.error(message);
  if (collected.errors.length > 0) return 2;

  const roots = args.dirs.map((input) => ({ input, real: realpathSync(resolve(cwd, input)) }));
  const failing = new Map<Invariant, string[]>(INVARIANTS.map((invariant) => [invariant, []]));
  const parses = new Map<string, { failed: string[]; total: number }>();
  const unreadable: string[] = [];
  const record: HashRecord = {};

  let files = 0;
  for (const path of collected.files) {
    let bytes: Uint8Array;
    try {
      bytes = readFileSync(path);
    } catch (error: unknown) {
      unreadable.push(`${shown(path, cwd)}: ${String(error)}`);
      continue;
    }

    const decoded = decode(bytes);
    const text = typeof decoded === "string" ? decoded : undefined;
    if (!args.includeGenerated && text !== undefined && isGeneratedHeader(text)) continue;

    files++;

    const extension = extname(path);
    const tally = parses.get(extension) ?? { failed: [], total: 0 };
    tally.total++;
    parses.set(extension, tally);

    const key = recordKey(path, roots);
    if (text === undefined) {
      tally.failed.push(shown(path, cwd));
      record[key] = { output: sha256(bytes), findings: "undecodable", explain: "undecodable" };
      continue;
    }

    const verdict = judge(path, text, args.braces ? false : keepBraces(path));
    if (verdict.kind === "parse failure") {
      tally.failed.push(shown(path, cwd));
      record[key] = { output: sha256(text), findings: "parse failure", explain: "parse failure" };
      continue;
    }

    for (const invariant of verdict.broken) failing.get(invariant)!.push(shown(path, cwd));

    record[key] = {
      output: verdict.output === undefined ? "crash" : sha256(verdict.output),
      findings: verdict.findings ?? "crash",
      explain: verdict.explain ?? "crash",
    };
  }

  if (files === 0) {
    for (const message of unreadable) console.error(message);

    console.error(
      `selected no files${args.includeGenerated ? "" : " (generated files are skipped, pass --include-generated to judge them)"}`,
    );

    return 2;
  }

  for (const [invariant, paths] of failing) {
    console.log(`${invariant}: ${paths.length}`);
    for (const path of paths) console.log(`  ${path}`);
  }

  console.log("parse failures:");

  for (const [extension, tally] of [...parses].sort(([left], [right]) => (left < right ? -1 : 1))) {
    console.log(`  ${extension} ${tally.failed.length}/${tally.total}`);
    for (const path of tally.failed) console.log(`    ${path}`);
  }

  console.log(`files: ${files}`);
  console.log(`elapsed: ${((performance.now() - started) / 1000).toFixed(2)}s`);

  const changed = previous ? printDifferences(previous, record) : 0;

  for (const message of unreadable) console.error(message);
  if (unreadable.length > 0) return 2;

  if (args.record?.mode === "snapshot")
    try {
      writeFileSync(recordFile!, `${JSON.stringify(sortedRecord(record), null, 2)}\n`);
    } catch (error: unknown) {
      console.error(String(error));
      return 2;
    }

  const broken = [...failing.values()].some((paths) => paths.length > 0);
  return broken || changed > 0 ? 1 : 0;
}

if (import.meta.main) process.exitCode = run();
