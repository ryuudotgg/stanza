import type { BlockStatement, Node } from "oxc-parser";
import { walk } from "./ast.ts";
import { addControlledBlocks, braceEdits } from "./braces.ts";
import { blankLines, document, lineAt, parseFinding } from "./doc.ts";
import { ignored, regions, within, type Region } from "./directives.ts";
import { applyLines, applyOffsets } from "./edits.ts";
import { spacing } from "./gaps.ts";
import { afterLines, afterOffsets, touching } from "./hunks.ts";
import { listAt, type List } from "./lists.ts";
import type { Doc } from "./model.ts";
import { parse } from "./parse.ts";
import type { FileResult, Finding, Mode, Options } from "./types.ts";

function sorted(findings: Finding[]): Finding[] {
  return findings.sort(
    (left, right) =>
      left.line - right.line || left.col - right.col || left.rule.localeCompare(right.rule),
  );
}

export interface Scan {
  lists: List[];
  blocks: BlockStatement[];
  owners: Map<BlockStatement, Node>;
  frozen: Region[];
}

export function scan(doc: Doc): Scan {
  const lists: List[] = [];
  const blocks: BlockStatement[] = [];
  const owners = new Map<BlockStatement, Node>();
  const chains = new Map<Node, Node>();
  const frozenOwners = new Set<Node>();

  walk(
    doc.program,
    (node, parent) => {
      const list = listAt(doc, node, parent);
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
          parent.type === "IfStatement" &&
          parent.alternate === node)
      )
        frozenOwners.add(node);
    },
    (node) => {
      if (frozenOwners.has(node)) return;

      const start = blocks.length;
      addControlledBlocks(node, blocks);

      for (let index = start; index < blocks.length; index++)
        owners.set(blocks[index]!, chains.get(node) ?? node);
    },
  );

  const frozen = regions(doc);
  for (const list of lists)
    for (const stmt of list.stmts) if (within(frozen, stmt.node.start)) stmt.frozen = true;

  return {
    lists: lists.filter((list) => !within(frozen, list.start)),
    blocks: blocks.filter((block) => !frozenOwners.has(block) && !within(frozen, block.start)),
    owners,
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

  return scanned.blocks.filter(
    (block) =>
      touches(lineAt(doc, block.start), lineAt(doc, block.end - 1)) ||
      owners.has(scanned.owners.get(block)!),
  );
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
      findings: sorted([
        ...braces.findings,
        ...spacing(doc, scanned.lists, scanned.frozen, touches).findings,
      ]),
      parseError: false,
    };

  let unbraced = text;
  let unbracedDoc = doc;
  let unbracedScan = scanned;
  for (
    let edits = braces.edits;
    edits.length > 0 && !options.keepBraces;
    edits = braceEdits(unbracedDoc, scopedBlocks(unbracedDoc, unbracedScan, touches)).edits
  ) {
    unbraced = applyOffsets(unbraced, edits);
    const next = document(path, unbraced, parse(path, unbraced));

    lines = afterOffsets(unbracedDoc, lines, edits, next);
    touches = touching(lines);

    unbracedDoc = next;
    unbracedScan = scan(unbracedDoc);
  }

  const spacingEdits = spacing(unbracedDoc, unbracedScan.lists, unbracedScan.frozen, touches).edits;

  const spaced = applyLines(unbracedDoc, spacingEdits);
  lines = afterLines(unbracedDoc, lines, spacingEdits);
  touches = touching(lines);

  const finalDoc = spaced === unbraced ? unbracedDoc : document(path, spaced, parse(path, spaced));
  const finalScan = finalDoc === unbracedDoc ? unbracedScan : scan(finalDoc);

  const findings = spacing(finalDoc, finalScan.lists, finalScan.frozen, touches).findings.filter(
    (item) => !item.fixable,
  );

  return { text: spaced, findings: sorted(findings), parseError: false };
}
