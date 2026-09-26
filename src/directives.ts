import type { Comment, Node } from "oxc-parser";
import { children } from "./ast.ts";
import { commentIndex, lineAt } from "./doc.ts";
import type { Doc } from "./model.ts";

export type Region = { start: number; end: number };

type Directive = "ignore" | "off" | "on";

function directive(comment: Comment): Directive | null {
  const match = /^stanza-(ignore|off|on)\b/.exec(comment.value.trim());
  return match ? (match[1] as Directive) : null;
}

function directlyAbove(doc: Doc, comment: Comment, top: number): boolean {
  const between = doc.text.slice(comment.end, top);
  const lineStart = doc.lineStarts[lineAt(doc, comment.start) - 1]!;
  return (
    /^[^\S\n]*\n[^\S\n]*$/.test(between) && /^\s*$/.test(doc.text.slice(lineStart, comment.start))
  );
}

export function ignored(doc: Doc, start: number): boolean {
  let top = start;
  for (let index = commentIndex(doc, start) - 1; index >= 0; index--) {
    const comment = doc.comments[index]!;
    if (!directlyAbove(doc, comment, top)) return false;
    if (directive(comment) === "ignore") return true;

    top = comment.start;
  }

  return false;
}

const BLOCKS = new Set([
  "BlockStatement",
  "StaticBlock",
  "SwitchStatement",
  "SwitchCase",
  "ClassBody",
  "TSModuleBlock",
]);

function enclosing(doc: Doc, comment: Comment): Region {
  let block: Region = { start: 0, end: doc.text.length };
  for (let node: Node | undefined = doc.program; node;) {
    if (BLOCKS.has(node.type)) block = { start: node.start, end: node.end };
    node = children(node)
      .map(([, child]) => child)
      .find((child) => child.start < comment.start && comment.end <= child.end);
  }

  return block;
}

export function regions(doc: Doc): Region[] {
  const marks = doc.comments.filter(
    (comment) => directive(comment) === "off" || directive(comment) === "on",
  );

  if (marks.length === 0) return [];

  const open = new Map<string, Region & { depth: number }>();
  const result: Region[] = [];
  for (const comment of marks) {
    const range = enclosing(doc, comment);
    const key = `${range.start}:${range.end}`;
    const current = open.get(key);
    if (directive(comment) === "off") {
      if (current) current.depth++;
      else open.set(key, { start: comment.start, end: range.end, depth: 1 });

      continue;
    }

    if (!current) continue;

    current.depth--;
    if (current.depth > 0) continue;

    result.push({ start: current.start, end: comment.start });
    open.delete(key);
  }

  for (const { start, end } of open.values()) result.push({ start, end });

  return result;
}

export function within(regions: Region[], offset: number): boolean {
  return regions.some((region) => region.start <= offset && offset < region.end);
}
