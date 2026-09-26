import type { BlockStatement, Comment, Node, Statement } from "oxc-parser";
import { JUMP_TYPES, LOOP_TYPES } from "./ast.ts";
import { commentIndex, finding, lineAt, nextToken, source } from "./doc.ts";
import type { OffsetEdit } from "./edits.ts";
import type { Doc } from "./model.ts";
import type { Finding } from "./types.ts";

const REMOVABLE = new Set(["ExpressionStatement", ...JUMP_TYPES, "IfStatement", ...LOOP_TYPES]);

export function addControlledBlocks(node: Node, blocks: BlockStatement[]): void {
  switch (node.type) {
    case "IfStatement":
      if (node.consequent.type === "BlockStatement") blocks.push(node.consequent);
      if (node.alternate?.type === "BlockStatement") blocks.push(node.alternate);
      return;

    case "ForStatement":
    case "ForInStatement":
    case "ForOfStatement":
    case "WhileStatement":
    case "DoWhileStatement":
      if (node.body.type === "BlockStatement") blocks.push(node.body);
  }
}

function endsWithOpenIf(node: Statement, removed: ReadonlySet<BlockStatement>): boolean {
  let current = node;
  while (true) {
    switch (current.type) {
      case "IfStatement":
        if (!current.alternate) return true;
        current = current.alternate;
        continue;

      case "ForStatement":
      case "ForInStatement":
      case "ForOfStatement":
      case "WhileStatement":
      case "DoWhileStatement":
      case "LabeledStatement":
      case "WithStatement":
        current = current.body;
        continue;

      case "BlockStatement":
        if (!removed.has(current)) return false;
        current = current.body[0]!;
        continue;

      default:
        return false;
    }
  }
}

function openingEdit(doc: Doc, offset: number): OffsetEdit {
  const line = lineAt(doc, offset);
  const start = doc.lineStarts[line - 1]!;
  const end = start + doc.lines[line - 1]!.length;
  if (doc.lines[line - 1]!.trim() === "{") return { start, end: end + 1 };
  if (doc.text.slice(offset + 1, end).trim() === "") {
    const horizontal = /[^\S\r\n]*$/.exec(doc.text.slice(start, offset))![0].length;
    return { start: offset - horizontal, end: offset + 1 };
  }

  return { start: offset, end: offset + (doc.text[offset + 1] === " " ? 2 : 1) };
}

function closingEdit(doc: Doc, offset: number): OffsetEdit {
  const line = lineAt(doc, offset);
  const start = doc.lineStarts[line - 1]!;
  const end = start + doc.lines[line - 1]!.length;
  if (doc.lines[line - 1]!.trim() === "}") {
    const finalLine = end === doc.text.length;
    return { start, end: finalLine ? end : end + 1 };
  }

  if (/^ \w/.test(doc.text.slice(offset + 1))) return { start: offset, end: offset + 2 };
  return { start: offset - (doc.text[offset - 1] === " " ? 1 : 0), end: offset + 1 };
}

function continuation(doc: Doc, inner: Statement, block: BlockStatement): number | null {
  if (source(doc, inner).endsWith(";")) return null;

  const after = nextToken(doc, block.end);
  const separated = /[\r\n]/.test(doc.text.slice(block.end, after));
  const safe = after >= doc.text.length || (separated && !/^[([`+\-/.<;]/.test(doc.text[after]!));
  return safe ? null : after;
}

function fusesIdentifiers(doc: Doc, edit: OffsetEdit): boolean {
  return (
    /[$\\\p{ID_Continue}\u200C\u200D]$/u.test(
      doc.text.slice(Math.max(0, edit.start - 2), edit.start),
    ) && /^[$\\\p{ID_Continue}\u200C\u200D]/u.test(doc.text.slice(edit.end, edit.end + 2))
  );
}

export type BraceHold =
  | { kind: "count"; count: number }
  | { kind: "statement"; inner: Statement }
  | { kind: "continues"; inner: Statement; after: number }
  | { kind: "comment"; comment: Comment }
  | { kind: "else"; inner: Statement }
  | { kind: "fuse" };

export function braceHold(
  doc: Doc,
  block: BlockStatement,
  removed: ReadonlySet<BlockStatement>,
): BraceHold | null {
  const inner = block.body[0];
  if (block.body.length !== 1 || !inner) return { kind: "count", count: block.body.length };
  if (!REMOVABLE.has(inner.type)) return { kind: "statement", inner };

  const after = continuation(doc, inner, block);
  if (after !== null) return { kind: "continues", inner, after };

  for (
    let index = commentIndex(doc, block.start + 1);
    index < commentIndex(doc, block.end);
    index++
  ) {
    const comment = doc.comments[index]!;
    if (comment.start < inner.start || comment.end > inner.end) return { kind: "comment", comment };
  }

  const next = nextToken(doc, block.end);
  if (/^else\b/.test(doc.text.slice(next)) && endsWithOpenIf(inner, removed))
    return { kind: "else", inner };

  const opening = openingEdit(doc, block.start);
  const closing = closingEdit(doc, block.end - 1);
  if (fusesIdentifiers(doc, opening) || fusesIdentifiers(doc, closing)) return { kind: "fuse" };

  return null;
}

export function braceEdits(
  doc: Doc,
  blocks: BlockStatement[],
): { edits: OffsetEdit[]; findings: Finding[] } {
  const edits: OffsetEdit[] = [];
  const findings: Finding[] = [];
  const removed = new Set<BlockStatement>();
  for (const block of blocks) {
    if (braceHold(doc, block, removed) !== null) continue;

    removed.add(block);
    edits.push(openingEdit(doc, block.start), closingEdit(doc, block.end - 1));
    findings.push(finding(doc, block.start, "braces"));
  }

  return { edits, findings };
}
