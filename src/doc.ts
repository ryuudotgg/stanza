import type { Node } from "oxc-parser";
import type { Doc } from "./model.ts";
import type { Parsed } from "./parse.ts";
import { RULES, type RuleId } from "./rules.ts";
import type { Finding } from "./types.ts";

export function document(path: string, text: string, parsed: Parsed): Doc {
  const lines = text.split("\n");
  const lineStarts: number[] = [];

  let offset = 0;
  for (const line of lines) {
    lineStarts.push(offset);
    offset += line.length + 1;
  }

  return { path, text, lines, lineStarts, program: parsed.program, comments: parsed.comments };
}

export function lineAt(doc: Doc, offset: number): number {
  let low = 0;
  let high = doc.lineStarts.length;
  while (low + 1 < high) {
    const middle = Math.floor((low + high) / 2);
    if (doc.lineStarts[middle]! <= offset) low = middle;
    else high = middle;
  }

  return low + 1;
}

function position(doc: Doc, offset: number): Pick<Finding, "path" | "line" | "col"> {
  const line = lineAt(doc, offset);
  return { path: doc.path, line, col: offset - doc.lineStarts[line - 1]! + 1 };
}

export function finding<R extends RuleId>(
  doc: Doc,
  offset: number,
  rule: R,
  ...args: Parameters<(typeof RULES)[R]["message"]>
): Finding {
  const { message, fixable } = RULES[rule];
  return {
    ...position(doc, offset),
    rule,
    message: (message as (...args: Parameters<(typeof RULES)[R]["message"]>) => string)(...args),
    fixable,
  };
}

export function parseFinding(doc: Doc, offset: number, message: string): Finding {
  return { ...position(doc, offset), rule: "parse", message, fixable: false };
}

export function source(doc: Doc, node: Pick<Node, "start" | "end">): string {
  return doc.text.slice(node.start, node.end);
}

export function blankLines(doc: Doc, after: number, before: number): number[] {
  const result: number[] = [];
  for (let line = after + 1; line < before; line++)
    if (doc.lines[line - 1]?.trim() === "") result.push(line);
  return result;
}

export function commentIndex(doc: Doc, offset: number): number {
  let low = 0;
  let high = doc.comments.length;
  while (low < high) {
    const middle = Math.floor((low + high) / 2);
    if (doc.comments[middle]!.start < offset) low = middle + 1;
    else high = middle;
  }

  return low;
}

export function nextToken(doc: Doc, offset: number): number {
  let cursor = offset;
  while (cursor < doc.text.length) {
    if (/\s/.test(doc.text[cursor]!)) {
      cursor++;
      continue;
    }

    const comment = doc.comments[commentIndex(doc, cursor)];
    if (comment?.start !== cursor) break;

    cursor = comment.end;
  }

  return cursor;
}
