import type { BlockStatement, Node } from "oxc-parser";
import { braceEdits, controlledBlocks } from "./braces.ts";
import { blankLines, document, lineAt, parseFinding } from "./doc.ts";
import { ignored, regions, within, type Region } from "./directives.ts";
import { applyLines, applyOffsets, type OffsetEdit } from "./edits.ts";
import { spacing } from "./gaps.ts";
import { afterLines, afterOffsets, touching } from "./hunks.ts";
import { listAt, type List } from "./lists.ts";
import type { Doc } from "./model.ts";
import { parse } from "./parse.ts";
import { compareFindings, type FileResult, type Mode, type Options } from "./types.ts";

export interface Scan {
  lists: List[];
  blocks: BlockStatement[];
  owners: Map<BlockStatement, Node>;
  chains: Map<Node, Node>;
  frozen: Region[];
}

export function scan(doc: Doc): Scan {
  const lists: List[] = [];
  const chains = new Map<Node, Node>();
  const frozenOwners = new Set<Node>();

  const owners = controlledBlocks(doc.program, (node, parent) => {
    const list = listAt(doc, node);
    if (list) lists.push(list);

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

  const frozen = regions(doc);
  for (const list of lists)
    for (const stmt of list.stmts) if (within(frozen, stmt.node.start)) stmt.frozen = true;

  return {
    lists: lists.filter((list) => !within(frozen, list.start)),
    blocks: [...owners]
      .filter(
        ([block, owner]) =>
          !frozenOwners.has(owner) && !frozenOwners.has(block) && !within(frozen, block.start),
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

function unbrace(
  doc: Doc,
  scanned: Scan,
  edits: OffsetEdit[],
  lines: ReadonlySet<number> | undefined,
  passes?: OffsetEdit[][],
): { doc: Doc; scanned: Scan; lines: ReadonlySet<number> | undefined } {
  let touches = touching(lines);
  for (; edits.length > 0; edits = braceEdits(doc, scopedBlocks(doc, scanned, touches)).edits) {
    passes?.push(edits);

    const text = applyOffsets(doc.text, edits);
    const next = document(doc.path, text, parse(doc.path, text));

    lines = afterOffsets(doc, lines, edits, next);
    touches = touching(lines);
    doc = next;
    scanned = scan(doc);
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

export function traceFix(original: Doc, keepBraces: boolean): Trace {
  const first = scan(original);
  const edits = keepBraces ? [] : braceEdits(original, first.blocks).edits;
  const passes: OffsetEdit[][] = [];
  const { doc, scanned } = unbrace(original, first, edits, undefined, passes);
  return { original, first, passes, doc, scanned };
}

export function processFile(path: string, text: string, mode: Mode, options: Options): FileResult {
  const parsed = parse(path, text);
  const doc = document(path, text, parsed);
  const error = parsed.errors[0];
  if (error)
    return {
      text,
      findings: [parseFinding(doc, error.labels[0]?.start ?? 0, error.message)],
      parseError: true,
    };

  const scanned = scan(doc);
  let lines = options.changedLines;
  let touches = touching(lines);
  const braces = options.keepBraces
    ? { edits: [], findings: [] }
    : braceEdits(doc, scopedBlocks(doc, scanned, touches));

  if (mode === "check")
    return {
      text,
      findings: [
        ...braces.findings,
        ...spacing(doc, scanned.lists, scanned.frozen, touches).findings,
      ].sort(compareFindings),
      parseError: false,
    };

  const unbraced = unbrace(doc, scanned, braces.edits, lines);
  const unbracedDoc = unbraced.doc;
  const unbracedScan = unbraced.scanned;
  lines = unbraced.lines;
  touches = touching(lines);

  const spacingEdits = spacing(unbracedDoc, unbracedScan.lists, unbracedScan.frozen, touches).edits;

  const spaced = applyLines(unbracedDoc, spacingEdits);
  lines = afterLines(unbracedDoc, lines, spacingEdits);
  touches = touching(lines);

  const finalDoc =
    spaced === unbracedDoc.text ? unbracedDoc : document(path, spaced, parse(path, spaced));

  const finalScan = finalDoc === unbracedDoc ? unbracedScan : scan(finalDoc);

  const findings = spacing(finalDoc, finalScan.lists, finalScan.frozen, touches).findings.filter(
    (item) => !item.fixable,
  );

  return { text: spaced, findings: findings.sort(compareFindings), parseError: false };
}
