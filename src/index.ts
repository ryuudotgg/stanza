import type { BlockStatement, Node } from "oxc-parser";
import { walk } from "./ast.ts";
import { addControlledBlocks, braceEdits } from "./braces.ts";
import { document, parseFinding } from "./doc.ts";
import { ignored, regions, within, type Region } from "./directives.ts";
import { applyLines, applyOffsets } from "./edits.ts";
import { spacing } from "./gaps.ts";
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

function scan(doc: Doc): { lists: List[]; blocks: BlockStatement[]; frozen: Region[] } {
  const lists: List[] = [];
  const blocks: BlockStatement[] = [];
  const frozenOwners = new Set<Node>();

  walk(
    doc.program,
    (node, parent) => {
      const list = listAt(doc, node, parent);
      if (list) lists.push(list);

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
      if (!frozenOwners.has(node)) addControlledBlocks(node, blocks);
    },
  );

  const frozen = regions(doc);
  for (const list of lists)
    for (const stmt of list.stmts) if (within(frozen, stmt.node.start)) stmt.frozen = true;

  return {
    lists: lists.filter((list) => !within(frozen, list.start)),
    blocks: blocks.filter((block) => !frozenOwners.has(block) && !within(frozen, block.start)),
    frozen,
  };
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
  const braces = options.keepBraces ? { edits: [], findings: [] } : braceEdits(doc, scanned.blocks);
  if (mode === "check")
    return {
      text,
      findings: sorted([
        ...braces.findings,
        ...spacing(doc, scanned.lists, scanned.frozen).findings,
      ]),
      parseError: false,
    };

  let unbraced = text;
  let unbracedDoc = doc;
  let unbracedScan = scanned;
  for (
    let edits = braces.edits;
    edits.length > 0 && !options.keepBraces;
    edits = braceEdits(unbracedDoc, unbracedScan.blocks).edits
  ) {
    unbraced = applyOffsets(unbraced, edits);
    unbracedDoc = document(path, unbraced, parse(path, unbraced));
    unbracedScan = scan(unbracedDoc);
  }

  const spaced = applyLines(
    unbracedDoc,
    spacing(unbracedDoc, unbracedScan.lists, unbracedScan.frozen).edits,
  );

  const finalDoc = spaced === unbraced ? unbracedDoc : document(path, spaced, parse(path, spaced));
  const finalScan = finalDoc === unbracedDoc ? unbracedScan : scan(finalDoc);

  const findings = spacing(finalDoc, finalScan.lists, finalScan.frozen).findings.filter(
    (item) => !item.fixable,
  );

  return { text: spaced, findings: sorted(findings), parseError: false };
}
