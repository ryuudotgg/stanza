import { createHash } from "node:crypto";
import { readFileSync, realpathSync, writeFileSync } from "node:fs";
import { extname, join, relative, resolve, sep } from "node:path";
import type { BlockStatement, Comment, Node } from "oxc-parser";
import { children, walk } from "../src/ast.ts";
import { controlledBlocks } from "../src/braces.ts";
import { collectFiles, isGeneratedHeader } from "../src/files.ts";
import { parse, type Parsed } from "../src/parse.ts";
import { decode, fixText, keepBraces, withoutMark } from "../src/step.ts";
import type { FileResult, Options } from "../src/types.ts";

export const INVARIANTS = [
  "idempotence",
  "preservation",
  "program shape",
  "fixable left",
  "directives",
  "full hunk",
  "empty hunk",
  "crash",
] as const;

export type Invariant = (typeof INVARIANTS)[number];

export type Fix = typeof fixText;

export type Verdict =
  | { kind: "parse failure" }
  | { kind: "judged"; output: string | undefined; broken: Invariant[] };

export interface Side {
  text: string;
  parsed: Parsed;
  blocks: BlockStatement[];
}

const POSITION_KEYS = new Set(["start", "end", "range", "loc"]);

export function side(path: string, text: string): Side {
  const body = withoutMark(text);
  const parsed = parse(path, body);
  const blocks = [...controlledBlocks(parsed.program).keys()];
  return { text: body, parsed, blocks };
}

const SEAM = "\0";

function seamedLines(side: Side): string[] {
  const offsets = side.blocks
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
  return side.parsed.comments.map((comment) => side.text.slice(comment.start, comment.end));
}

function sameItems(left: string[], right: string[]): boolean {
  return left.length === right.length && left.every((item, index) => item === right[index]);
}

function shape(side: Side): string | undefined {
  if (side.parsed.errors.length > 0) return undefined;

  const flattened = new Set(side.blocks.filter((block) => block.body.length === 1));
  return JSON.stringify(side.parsed.program, (key, value) => {
    if (POSITION_KEYS.has(key)) return undefined;
    if (typeof value === "bigint") return `${value}n`;
    return flattened.has(value) ? value.body[0] : value;
  });
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
  const fixedShape = shape(fixed);
  return fixedShape !== undefined && fixedShape === shape(original);
}

export function leavesNothingFixable(
  path: string,
  fixed: string,
  options: Options,
  fix: Fix = fixText,
): boolean {
  return !fix(path, fixed, "check", options).findings.some((finding) => finding.fixable);
}

export function coversEveryLine(
  path: string,
  text: string,
  fixed: string,
  options: Options,
  fix: Fix = fixText,
): boolean {
  const changedLines = new Set(
    Array.from({ length: text.split("\n").length }, (_, index) => index + 1),
  );

  return fix(path, text, "fix", { ...options, changedLines }).text === fixed;
}

export function touchesNoLine(
  path: string,
  text: string,
  options: Options,
  fix: Fix = fixText,
): boolean {
  return fix(path, text, "fix", { ...options, changedLines: new Set() }).text === text;
}

type Directive = { kind: "ignore" | "off" | "on"; comment: Comment };
type Range = { start: number; end: number };
type Statement = { node: Node; parent: Node | null };

const ENCLOSING = new Set([
  "BlockStatement",
  "StaticBlock",
  "SwitchStatement",
  "SwitchCase",
  "ClassBody",
  "TSModuleBlock",
]);

const INTERIORS = new Set(["BlockStatement", "StaticBlock", "ClassBody", "TSModuleBlock"]);

const STATEMENT_ARRAYS = new Set([
  "Program:body",
  "BlockStatement:body",
  "StaticBlock:body",
  "SwitchStatement:cases",
  "SwitchCase:consequent",
  "TSModuleBlock:body",
]);

function directives(side: Side): Directive[] {
  return side.parsed.comments.flatMap((comment) => {
    const match = /^stanza-(ignore|off|on)\b/.exec(comment.value.trim());
    return match ? [{ kind: match[1] as Directive["kind"], comment }] : [];
  });
}

function enclosingNode(side: Side, comment: Comment): Node {
  let enclosing: Node = side.parsed.program;
  walk(side.parsed.program, (node) => {
    if (ENCLOSING.has(node.type) && node.start < comment.start && comment.end <= node.end)
      if (node.end - node.start < enclosing.end - enclosing.start) enclosing = node;
  });

  return enclosing;
}

function offSpan(side: Side, marks: Directive[], index: number): string {
  const comment = marks[index]!.comment;
  const enclosing = enclosingNode(side, comment);

  let depth = 1;
  let end = enclosing.end;
  for (const mark of marks.slice(index + 1)) {
    if (mark.comment.start >= enclosing.end) break;
    if (mark.kind === "ignore" || enclosingNode(side, mark.comment) !== enclosing) continue;

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

function statementAt(side: Side, comment: Comment): Statement | undefined {
  let top = comment;
  for (const next of side.parsed.comments) {
    if (next.start <= top.start) continue;
    if (!directlyAbove(side, top, next.start)) break;
    top = next;
  }

  let next: Statement | undefined;
  walk(side.parsed.program, (node, parent) => {
    if (
      node.start < top.end ||
      (node.type !== "SwitchCase" && !/(Statement|Declaration)$/.test(node.type))
    )
      return;

    if (!next || node.start < next.node.start) next = { node, parent };
  });

  if (!next || !directlyAbove(side, top, next.node.start)) return undefined;

  return next;
}

function controlledRanges(node: Node): Range[] {
  const ranges: Range[] = [];
  function body(child: Node): void {
    ranges.push(
      child.type === "BlockStatement"
        ? { start: child.start + 1, end: child.end - 1 }
        : { start: child.start, end: child.end },
    );
  }

  function chain(current: Node): void {
    switch (current.type) {
      case "LabeledStatement":
        chain(current.body);
        break;

      case "IfStatement":
        body(current.consequent);

        if (current.alternate?.type === "IfStatement") chain(current.alternate);
        else if (current.alternate) body(current.alternate);

        break;

      case "ForStatement":
      case "ForInStatement":
      case "ForOfStatement":
      case "WhileStatement":
      case "DoWhileStatement":
      case "WithStatement":
        body(current.body);
    }
  }

  chain(node);
  return ranges;
}

function statementSkeleton(side: Side, statement: Statement): string {
  const ranges = controlledRanges(statement.node);

  walk(statement.node, (node) => {
    if (ranges.some((range) => range.start <= node.start && node.end <= range.end)) return;
    if (INTERIORS.has(node.type)) ranges.push({ start: node.start + 1, end: node.end - 1 });
    else if (node.type === "SwitchStatement")
      ranges.push({ start: node.discriminant.end, end: node.end - 1 });
  });

  const outermost = ranges
    .sort((left, right) => left.start - right.start || right.end - left.end)
    .filter(
      (range, index, all) =>
        !all.slice(0, index).some((prior) => prior.start <= range.start && range.end <= prior.end),
    );

  let cursor = statement.node.start;
  let result = "";
  for (const range of outermost) {
    result += side.text.slice(cursor, range.start) + "\0";
    cursor = range.end;
  }

  return result + side.text.slice(cursor, statement.node.end);
}

function edgeAbove(parent: Node): number | undefined {
  if (parent.type === "BlockStatement" || parent.type === "StaticBlock") return parent.start;
  if (parent.type === "SwitchStatement") return parent.discriminant.end;
}

function edgeBelow(parent: Node): number | undefined {
  if (["BlockStatement", "StaticBlock", "SwitchStatement"].includes(parent.type)) return parent.end;
}

function statementGaps(original: Side, left: Statement, fixed: Side, right: Statement): boolean {
  function gaps(side: Side, statement: Statement): [string | undefined, string | undefined] {
    if (!statement.parent) return [undefined, undefined];

    const entry = children(statement.parent).find(([, child]) => child === statement.node);
    if (!entry || !STATEMENT_ARRAYS.has(`${statement.parent.type}:${entry[0]}`))
      return [undefined, undefined];

    const siblings = children(statement.parent)
      .filter(([key]) => key === entry[0])
      .map(([, child]) => child);

    const index = siblings.indexOf(statement.node);
    const previous = siblings[index - 1];
    const next = siblings[index + 1];

    const above = previous?.end ?? edgeAbove(statement.parent);
    const below = next?.start ?? edgeBelow(statement.parent);
    return [
      above === undefined ? undefined : side.text.slice(above, statement.node.start),
      below === undefined ? undefined : side.text.slice(statement.node.end, below),
    ];
  }

  const first = gaps(original, left);
  const second = gaps(fixed, right);
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
    if (
      !counterpart ||
      (statement.node.type !== "SwitchCase" &&
        statementSkeleton(original, statement) !== statementSkeleton(fixed, counterpart))
    )
      return false;

    if (!statementGaps(original, statement, fixed, counterpart)) return false;
  }

  return true;
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
    return { kind: "judged", output: undefined, broken: ["crash"] };
  }

  if (result.parseError) return { kind: "parse failure" };

  const output = result.text;

  let sides: { original: Side; fixed: Side };
  try {
    sides = { original: side(path, text), fixed: side(path, output) };
  } catch {
    return { kind: "judged", output, broken: ["crash"] };
  }

  const checks: [Invariant, () => boolean][] = [
    ["idempotence", () => isIdempotent(path, output, options, fix)],
    ["preservation", () => preservesText(sides.original, sides.fixed)],
    ["program shape", () => preservesShape(sides.original, sides.fixed)],
    ["fixable left", () => leavesNothingFixable(path, output, options, fix)],
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

  if (crashed) broken.push("crash");
  return { kind: "judged", output, broken };
}

type HashRecord = { [path: string]: string };

interface Arguments {
  dirs: string[];
  includeGenerated: boolean;
  record?: { mode: "snapshot" | "against"; file: string };
}

const usage =
  "Usage: bun scripts/corpus.ts [--snapshot <file> | --against <file>] [--include-generated] <dir>...";

function parseArguments(args: string[]): Arguments | undefined {
  const dirs: string[] = [];

  let record: Arguments["record"];
  let includeGenerated = false;
  for (let index = 0; index < args.length; index++) {
    const arg = args[index]!;
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

  return dirs.length > 0 ? { dirs, record, includeGenerated } : undefined;
}

function sha256(content: string | Uint8Array): string {
  return createHash("sha256").update(content).digest("hex");
}

function readRecord(file: string): HashRecord {
  const raw: unknown = JSON.parse(readFileSync(file, "utf8"));
  const valid =
    typeof raw === "object" &&
    raw !== null &&
    !Array.isArray(raw) &&
    Object.values(raw).every((value) => typeof value === "string");

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
  if (previous[path] !== current[path]) return "differs";
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
      record[key] = sha256(bytes);
      continue;
    }

    const verdict = judge(path, text, keepBraces(path));
    if (verdict.kind === "parse failure") {
      tally.failed.push(shown(path, cwd));
      record[key] = sha256(text);
      continue;
    }

    for (const invariant of verdict.broken) failing.get(invariant)!.push(shown(path, cwd));

    record[key] = verdict.output === undefined ? "crash" : sha256(verdict.output);
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
