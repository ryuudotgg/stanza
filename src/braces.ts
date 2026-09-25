import type { BlockStatement, Node, Statement } from "oxc-parser";
import { children } from "./ast.ts";
import { commentIndex, finding, lineAt, nextToken, source } from "./doc.ts";
import type { OffsetEdit } from "./edits.ts";
import type { Doc } from "./model.ts";
import type { Finding } from "./types.ts";

const REMOVABLE = new Set([
  "ExpressionStatement",
  "ReturnStatement",
  "ThrowStatement",
  "BreakStatement",
  "ContinueStatement",
  "IfStatement",
  "ForStatement",
  "ForInStatement",
  "ForOfStatement",
  "WhileStatement",
  "DoWhileStatement",
]);

function controlledBlocks(node: Node): BlockStatement[] {
  if (node.type === "IfStatement")
    return [node.consequent, node.alternate].filter(
      (body): body is BlockStatement => body?.type === "BlockStatement",
    );

  if (
    node.type === "ForStatement" ||
    node.type === "ForInStatement" ||
    node.type === "ForOfStatement" ||
    node.type === "WhileStatement" ||
    node.type === "DoWhileStatement"
  )
    return node.body.type === "BlockStatement" ? [node.body] : [];

  return [];
}

function endsWithOpenIf(node: Statement, removed: Set<BlockStatement>): boolean {
  switch (node.type) {
    case "IfStatement":
      return node.alternate ? endsWithOpenIf(node.alternate, removed) : true;

    case "ForStatement":
    case "ForInStatement":
    case "ForOfStatement":
    case "WhileStatement":
    case "DoWhileStatement":
    case "LabeledStatement":
    case "WithStatement":
      return endsWithOpenIf(node.body, removed);

    case "BlockStatement":
      return removed.has(node) && endsWithOpenIf(node.body[0]!, removed);

    default:
      return false;
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

function endsSafely(doc: Doc, inner: Statement, block: BlockStatement): boolean {
  const text = source(doc, inner);
  if (text.endsWith(";")) return true;

  const after = nextToken(doc, block.end);
  const separated = /[\r\n]/.test(doc.text.slice(block.end, after));
  return after >= doc.text.length || (separated && !/^[([`+\-/.<]/.test(doc.text[after]!));
}

function fusesIdentifiers(doc: Doc, edit: OffsetEdit): boolean {
  return (
    /[$\\\p{ID_Continue}\u200C\u200D]$/u.test(doc.text.slice(0, edit.start)) &&
    /^[$\\\p{ID_Continue}\u200C\u200D]/u.test(doc.text.slice(edit.end))
  );
}

function removable(doc: Doc, block: BlockStatement, removed: Set<BlockStatement>): boolean {
  const inner = block.body[0];
  if (block.body.length !== 1 || !inner || !REMOVABLE.has(inner.type)) return false;
  if (!endsSafely(doc, inner, block)) return false;

  for (
    let index = commentIndex(doc, block.start + 1);
    index < commentIndex(doc, block.end);
    index++
  ) {
    const comment = doc.comments[index]!;
    if (comment.start < inner.start || comment.end > inner.end) return false;
  }

  const after = nextToken(doc, block.end);
  if (/^else\b/.test(doc.text.slice(after)) && endsWithOpenIf(inner, removed)) return false;

  return true;
}

export function braceEdits(doc: Doc): { edits: OffsetEdit[]; findings: Finding[] } {
  const edits: OffsetEdit[] = [];
  const findings: Finding[] = [];
  const removed = new Set<BlockStatement>();
  function visit(node: Node): void {
    for (const [, child] of children(node)) visit(child);

    for (const block of controlledBlocks(node)) {
      if (!removable(doc, block, removed)) continue;

      const opening = openingEdit(doc, block.start);
      const closing = closingEdit(doc, block.end - 1);
      if (fusesIdentifiers(doc, opening) || fusesIdentifiers(doc, closing)) continue;

      removed.add(block);
      edits.push(opening, closing);
      findings.push(
        finding(doc, block.start, "braces", "braces around a single statement body", true),
      );
    }
  }

  visit(doc.program);
  return { edits, findings };
}
