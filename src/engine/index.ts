import { languageOf } from "../languages/index.ts";
import type { BracePass, Language, Parsed, Scan, Touches } from "../languages/language.ts";
import { blankLines, document, lineAt, parseFinding, position } from "./doc.ts";
import { applyLines, applyOffsets, offsetMap, type OffsetEdit } from "./edits.ts";
import { spacing } from "./gaps.ts";
import { afterLines, afterOffsets, touching } from "./hunks.ts";
import type { Changed, Doc, LineEdits, StatementList, Stmt } from "./model.ts";
import { RULES, type RuleId } from "./rules.ts";
import {
  compareFindings,
  type FileResult,
  type Finding,
  type Mode,
  type Options,
} from "./types.ts";

function touchedStatements(doc: Doc, lists: StatementList[], touches: Touches): Set<Stmt> {
  const touched = new Set<Stmt>();
  for (const list of lists)
    for (let index = 1; index < list.stmts.length; index++) {
      const prev = list.stmts[index - 1]!;
      const next = list.stmts[index]!;
      if (!touches(prev.startLine, next.endLine)) continue;

      const shortBodyMayJoin =
        list.stmts.length <= 3 && blankLines(doc, prev.endLine, next.startLine).length > 0;

      for (const stmt of shortBodyMayJoin ? list.stmts : [prev, next]) touched.add(stmt);
    }

  return touched;
}

function bracePass(doc: Doc, scanned: Scan, touches: Touches, collapseChains: boolean): BracePass {
  const touched = touchedStatements(doc, scanned.lists, touches);
  return scanned.braces?.pass(touches, touched, collapseChains) ?? NO_BRACES;
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
  language: Language,
  doc: Doc,
  scanned: Scan,
  pass: BracePass,
  lines: Changed | undefined,
  trail: Trail,
  collapseChains: boolean,
  width: Options["width"],
  passes?: OffsetEdit[][],
): { doc: Doc; scanned: Scan; lines: Changed | undefined } {
  let touches = touching(lines);
  for (; pass.edits.length > 0; pass = bracePass(doc, scanned, touches, collapseChains)) {
    passes?.push(pass.edits);
    record(trail, doc, pass.findings);
    retrace(trail, offsetMap(pass.edits).back);

    const text = applyOffsets(doc.text, pass.edits);
    const parsed = language.parse(doc.path, text);
    const next = document(doc.path, text, parsed.comments);

    lines = afterOffsets(doc, lines, pass.edits, next);
    touches = touching(lines);
    doc = next;
    scanned = parsed.scan(doc, width);
  }

  return { doc, scanned, lines };
}

export interface Trace {
  original: Doc;
  first: Scan;
  passes: OffsetEdit[][];
  doc: Doc;
  scanned: Scan;
}

const NO_BRACES: BracePass = { edits: [], findings: [] };

export function traceFix(
  language: Language,
  original: Doc,
  parsed: Parsed,
  keepBraces: boolean,
  width: Options["width"],
): Trace {
  const first = parsed.scan(original, width);
  const pass = keepBraces ? NO_BRACES : bracePass(original, first, touching(undefined), false);
  const passes: OffsetEdit[][] = [];
  const { doc, scanned } = unbrace(
    language,
    original,
    first,
    pass,
    undefined,
    startTrail(original),
    false,
    width,
    passes,
  );

  return { original, first, passes, doc, scanned };
}

export function processFile(path: string, text: string, mode: Mode, options: Options): FileResult {
  const language = languageOf(path);
  const parsed = language.parse(path, text);
  const doc = document(path, text, parsed.comments);
  const rejected = parsed.rejection(text);
  if (rejected)
    return {
      text,
      findings: [parseFinding(doc, rejected.start, rejected.message)],
      parseError: true,
    };

  const scanned = parsed.scan(doc, options.width);
  let lines = options.changedLines;
  let touches = touching(lines);
  const trail = startTrail(doc);

  let last = doc;
  let lastScan = scanned;
  let settled: Finding[] | undefined;
  let pass = options.keepBraces ? NO_BRACES : bracePass(doc, scanned, touches, lines === undefined);
  while (true) {
    const unbraced = unbrace(
      language,
      last,
      lastScan,
      pass,
      lines,
      trail,
      lines === undefined,
      options.width,
    );

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

    const spacedParse = language.parse(path, spacedText);
    last = document(path, spacedText, spacedParse.comments);
    lastScan = spacedParse.scan(last, options.width);
    retrace(trail, lineBack(unbraced.doc, last, spaced.edits));
    if (lines === undefined || options.keepBraces) break;

    pass = bracePass(last, lastScan, touches, false);
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
