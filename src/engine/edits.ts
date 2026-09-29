import type { Doc, LineEdits } from "./model.ts";

export interface OffsetEdit {
  start: number;
  end: number;
}

export interface OffsetMap {
  forward(offset: number): number;
  back(offset: number): number;
  removes(offset: number): boolean;
}

export function lowerBound(values: readonly number[], target: number): number {
  let low = 0;
  let high = values.length;
  while (low < high) {
    const middle = Math.floor((low + high) / 2);
    if (values[middle]! < target) low = middle + 1;
    else high = middle;
  }

  return low;
}

export function offsetMap(edits: OffsetEdit[]): OffsetMap {
  const ordered = edits.toSorted((left, right) => left.start - right.start);
  const starts = ordered.map((edit) => edit.start);
  const removedBefore = [0];
  const shiftedStarts: number[] = [];
  for (const edit of ordered) {
    shiftedStarts.push(edit.start - removedBefore.at(-1)!);
    removedBefore.push(removedBefore.at(-1)! + edit.end - edit.start);
  }

  return {
    forward(offset) {
      const index = lowerBound(starts, offset);
      if (index === 0) return offset;

      const edit = ordered[index - 1]!;
      return offset - removedBefore[index - 1]! - (Math.min(offset, edit.end) - edit.start);
    },
    back(offset) {
      const index = lowerBound(shiftedStarts, offset + 1);
      return offset + removedBefore[index]!;
    },
    removes(offset) {
      const index = lowerBound(starts, offset + 1);
      return index > 0 && offset < ordered[index - 1]!.end;
    },
  };
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
