import type { BlockStatement, Node, Program } from "oxc-parser";
import { braceEdits, controlledBlocks, type BracePass } from "./braces.ts";
import { blankLines, document, lineAt, parseFinding, position } from "./doc.ts";
import { ignored, marks, regions, within, type Region } from "./directives.ts";
import { applyLines, applyOffsets, offsetMap, type OffsetEdit } from "./edits.ts";
import { spacing } from "./gaps.ts";
import { afterLines, afterOffsets, touching } from "./hunks.ts";
import { listAt, type List } from "./lists.ts";
import type { Changed, Doc, LineEdits } from "./model.ts";
import { parse, rejection } from "./parse.ts";
import { RULES, type RuleId } from "./rules.ts";
import {
  compareFindings,
  type FileResult,
  type Finding,
  type Mode,
  type Options,
} from "./types.ts";

export interface Scan {
  lists: List[];
  blocks: BlockStatement[];
  owners: Map<BlockStatement, Node>;
  chains: Map<Node, Node>;
  frozen: Region[];
}

const CONTAINERS = new Set([
  "BlockStatement",
  "StaticBlock",
  "SwitchStatement",
  "SwitchCase",
  "ClassBody",
  "TSModuleBlock",
]);

export function scan(doc: Doc<Program>): Scan {
  const marked = marks(doc);
  const containers: Region[] = [];
  const escaped = new Set<Node>();

  const lists: List[] = [];
  const chains = new Map<Node, Node>();
  const frozenOwners = new Set<Node>();

  const owners = controlledBlocks(doc.program, (node, parent) => {
    const list = listAt(doc, node);
    if (list) lists.push(list);

    if (marked.length > 0)
      if (
        parent !== null &&
        (escaped.has(parent) || node.start < parent.start || parent.end < node.end)
      )
        escaped.add(node);
      else if (CONTAINERS.has(node.type)) containers.push({ start: node.start, end: node.end });

    if (node.type === "IfStatement")
      chains.set(
        node,
        parent?.type === "IfStatement" && parent.alternate === node
          ? (chains.get(parent) ?? parent)
          : node,
      );

    if (
      (node.type.endsWith("Statement") && ignored(doc, node.start)) ||
      (parent !== null &&
        frozenOwners.has(parent) &&
        ((parent.type === "IfStatement" && parent.alternate === node) ||
          parent.type === "LabeledStatement"))
    )
      frozenOwners.add(node);
  });

  const frozen = regions(doc, marked, containers);
  for (const list of lists)
    for (const stmt of list.stmts) if (within(frozen, stmt.start)) stmt.frozen = true;

  return {
    lists: lists.filter((list) => !within(frozen, list.start)),
    blocks: [...owners]
      .filter(
        ([block, owner]) =>
          !frozenOwners.has(owner) &&
          !frozenOwners.has(block) &&
          !within(frozen, block.start) &&
          !within(frozen, block.end - 1),
      )
      .map(([block]) => block),
    owners,
    chains,
    frozen,
  };
}

function scopedBlocks(
  doc: Doc,
  scanned: Scan,
  touches: (first: number, last: number) => boolean,
): BlockStatement[] {
  const owners = new Set<Node>();
  for (const list of scanned.lists)
    for (let index = 1; index < list.stmts.length; index++) {
      const prev = list.stmts[index - 1]!;
      const next = list.stmts[index]!;
      if (!touches(prev.startLine, next.endLine)) continue;

      const shortBodyMayJoin =
        list.stmts.length <= 3 && blankLines(doc, prev.endLine, next.startLine).length > 0;

      for (const stmt of shortBodyMayJoin ? list.stmts : [prev, next]) {
        let node: Node = stmt.node;
        while (node.type === "LabeledStatement") node = node.body;
        owners.add(node);
      }
    }

  return scanned.blocks.filter((block) => {
    const owner = scanned.owners.get(block)!;
    return (
      touches(lineAt(doc, block.start), lineAt(doc, block.end - 1)) ||
      owners.has(scanned.chains.get(owner) ?? owner)
    );
  });
}

interface Trail {
  input: Doc;
  back: (offset: number) => number;
  findings: Finding[];
}

function startTrail(input: Doc): Trail {
  return { input, back: (offset) => offset, findings: [] };
}

function record(trail: Trail, doc: Doc, findings: Finding[]): void {
  for (const item of findings) {
    const offset = trail.back(doc.lineStarts[item.line - 1]! + item.col - 1);
    trail.findings.push({ ...item, ...position(trail.input, offset) });
  }
}

function retrace(trail: Trail, step: (offset: number) => number): void {
  const back = trail.back;
  trail.back = (offset) => back(step(offset));
}

function lineBack(before: Doc, after: Doc, edits: LineEdits): (offset: number) => number {
  const origins: number[] = [];
  for (let line = 1; line <= before.lines.length; line++) {
    if (!edits.deleteLines.has(line)) origins.push(line);
    if (edits.insertAfter.has(line)) origins.push(Math.min(line + 1, before.lines.length));
  }

  return (offset) => {
    const line = lineAt(after, offset);
    const origin = origins[line - 1]!;
    const column = Math.min(offset - after.lineStarts[line - 1]!, before.lines[origin - 1]!.length);
    return before.lineStarts[origin - 1]! + column;
  };
}

function gapBlanks(doc: Doc, scanned: Scan, back: (offset: number) => number): Map<number, number> {
  const blanks = new Map<number, number>();
  for (const list of scanned.lists)
    for (let index = 1; index < list.stmts.length; index++) {
      const prev = list.stmts[index - 1]!;
      const next = list.stmts[index]!;
      blanks.set(back(next.start), blankLines(doc, prev.endLine, next.startLine).length);
    }

  return blanks;
}

function netGaps(trail: Trail, first: Scan, final: Doc, finalScan: Scan): Finding[] {
  const { input, findings } = trail;
  const offsetOf = (item: Finding) => input.lineStarts[item.line - 1]! + item.col - 1;
  const gapOf = (item: Finding) => (item.rule in RULES ? RULES[item.rule as RuleId].gap : null);

  const gaps = new Map<number, Finding[]>();
  for (const item of findings)
    if (gapOf(item) !== null) gaps.set(offsetOf(item), [...(gaps.get(offsetOf(item)) ?? []), item]);

  if ([...gaps.values()].every((group) => group.length === 1)) return findings;

  const before = gapBlanks(input, first, (offset) => offset);
  const after = gapBlanks(final, finalScan, trail.back);
  return findings.filter((item) => {
    const offset = offsetOf(item);
    const group = gaps.get(offset);
    if (!group || group.length === 1) return true;

    const change = Math.sign((after.get(offset) ?? 0) - (before.get(offset) ?? 0));
    const want = change > 0 ? "at-least-one" : "none";
    return change !== 0 && item === group.findLast((other) => gapOf(other) === want);
  });
}

function unbrace(
  doc: Doc<Program>,
  scanned: Scan,
  pass: BracePass,
  lines: Changed | undefined,
  trail: Trail,
  passes?: OffsetEdit[][],
): { doc: Doc<Program>; scanned: Scan; lines: Changed | undefined } {
  let touches = touching(lines);
  for (; pass.edits.length > 0; pass = braceEdits(doc, scopedBlocks(doc, scanned, touches))) {
    passes?.push(pass.edits);
    record(trail, doc, pass.findings);
    retrace(trail, offsetMap(pass.edits).back);

    const text = applyOffsets(doc.text, pass.edits);
    const next = document(doc.path, text, parse(doc.path, text));

    lines = afterOffsets(doc, lines, pass.edits, next);
    touches = touching(lines);
    doc = next;
    scanned = scan(doc);
  }

  return { doc, scanned, lines };
}

export interface Trace {
  original: Doc<Program>;
  first: Scan;
  passes: OffsetEdit[][];
  doc: Doc<Program>;
  scanned: Scan;
}

const NO_BRACES: BracePass = { edits: [], findings: [] };

export function traceFix(original: Doc<Program>, keepBraces: boolean): Trace {
  const first = scan(original);
  const pass = keepBraces ? NO_BRACES : braceEdits(original, first.blocks);
  const passes: OffsetEdit[][] = [];
  const { doc, scanned } = unbrace(original, first, pass, undefined, startTrail(original), passes);
  return { original, first, passes, doc, scanned };
}

export function processFile(path: string, text: string, mode: Mode, options: Options): FileResult {
  const parsed = parse(path, text);
  const doc = document(path, text, parsed);
  const rejected = rejection(text, parsed);
  if (rejected)
    return {
      text,
      findings: [parseFinding(doc, rejected.start, rejected.message)],
      parseError: true,
    };

  const scanned = scan(doc);
  let lines = options.changedLines;
  let touches = touching(lines);
  const trail = startTrail(doc);

  let last = doc;
  let lastScan = scanned;
  let settled: Finding[] | undefined;
  let pass = options.keepBraces ? NO_BRACES : braceEdits(doc, scopedBlocks(doc, scanned, touches));
  while (true) {
    const unbraced = unbrace(last, lastScan, pass, lines, trail);
    lines = unbraced.lines;
    touches = touching(lines);

    const spaced = spacing(unbraced.doc, unbraced.scanned.lists, unbraced.scanned.frozen, touches);
    record(
      trail,
      unbraced.doc,
      spaced.findings.filter((item) => item.fixable),
    );

    const spacedText = applyLines(unbraced.doc, spaced.edits);
    lines = afterLines(unbraced.doc, lines, spaced.edits);
    touches = touching(lines);

    if (spacedText === unbraced.doc.text || (mode === "check" && lines === undefined)) {
      last = unbraced.doc;
      lastScan = unbraced.scanned;
      settled = spaced.findings;
      break;
    }

    last = document(path, spacedText, parse(path, spacedText));
    lastScan = scan(last);
    retrace(trail, lineBack(unbraced.doc, last, spaced.edits));
    if (lines === undefined || options.keepBraces) break;

    pass = braceEdits(last, scopedBlocks(last, lastScan, touches));
    if (pass.edits.length === 0) break;
  }

  const remaining = (
    settled ?? spacing(last, lastScan.lists, lastScan.frozen, touches).findings
  ).filter((item) => !item.fixable);

  if (mode === "fix")
    return { text: last.text, findings: remaining.sort(compareFindings), parseError: false };

  record(trail, last, remaining);
  return {
    text,
    findings: netGaps(trail, scanned, last, lastScan).sort(compareFindings),
    parseError: false,
  };
}
