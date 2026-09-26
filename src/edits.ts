import type { Doc, LineEdits } from "./model.ts";

export interface OffsetEdit {
  start: number;
  end: number;
}

export function applyOffsets(text: string, edits: OffsetEdit[]): string {
  const parts: string[] = [];

  let cursor = 0;
  for (const edit of edits.toSorted((left, right) => left.start - right.start)) {
    if (edit.start >= cursor) parts.push(text.slice(cursor, edit.start));
    cursor = Math.max(cursor, edit.end);
  }

  let result = [...parts, text.slice(cursor)].join("");
  if (!text.endsWith("\n") && result.endsWith("\n"))
    result = result.slice(0, result.endsWith("\r\n") ? -2 : -1);

  return result;
}

export function applyLines(doc: Doc, edits: LineEdits): string {
  const lines: string[] = [];
  for (let index = 0; index < doc.lines.length; index++) {
    if (!edits.deleteLines.has(index + 1)) lines.push(doc.lines[index]!);
    if (edits.insertAfter.has(index + 1)) lines.push(doc.lines[index]!.endsWith("\r") ? "\r" : "");
  }

  return lines.join("\n");
}
