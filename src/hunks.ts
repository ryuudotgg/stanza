import { lineAt } from "./doc.ts";
import { lowerBound, offsetMap, type OffsetEdit } from "./edits.ts";
import type { Doc, LineEdits } from "./model.ts";

export function touching(
  lines: ReadonlySet<number> | undefined,
): (first: number, last: number) => boolean {
  if (lines === undefined) return () => true;
  const ordered = [...lines].sort((left, right) => left - right);
  return (first, last) => (ordered[lowerBound(ordered, first)] ?? Infinity) <= last;
}

function markAround(next: Doc, offset: number, carried: Set<number>): void {
  const line = lineAt(next, Math.min(offset, next.text.length));
  carried.add(line - 1);
  carried.add(line);
}

export function afterOffsets(
  doc: Doc,
  lines: ReadonlySet<number> | undefined,
  edits: OffsetEdit[],
  next: Doc,
): ReadonlySet<number> | undefined {
  if (lines === undefined) return undefined;

  const shift = offsetMap(edits).forward;
  const carried = new Set<number>();
  for (const line of lines) {
    const from = doc.lineStarts[line - 1];
    if (from === undefined) continue;

    const start = shift(from);
    const end = shift(doc.lineStarts[line] ?? doc.text.length);
    if (start === end) {
      markAround(next, start, carried);
      continue;
    }

    const last = lineAt(next, Math.min(end, next.text.length) - 1);
    for (let number = lineAt(next, start); number <= last; number++) carried.add(number);
  }

  for (const edit of edits) {
    const offset = shift(edit.start);
    if (doc.text.slice(edit.start, edit.end).includes("\n")) markAround(next, offset, carried);
    else carried.add(lineAt(next, Math.min(offset, next.text.length)));
  }

  carried.delete(0);
  return carried;
}

export function afterLines(
  doc: Doc,
  lines: ReadonlySet<number> | undefined,
  edits: LineEdits,
): ReadonlySet<number> | undefined {
  if (lines === undefined) return undefined;

  const carried = new Set<number>();

  let output = 0;
  let previous: number | undefined;
  let deleted = false;
  for (let line = 1; line <= doc.lines.length; line++) {
    if (edits.deleteLines.has(line)) {
      if (lines.has(line)) {
        deleted = true;
        if (previous !== undefined) carried.add(previous);
      }
    } else {
      output++;
      if (lines.has(line) || deleted) carried.add(output);

      previous = output;
      deleted = false;
    }

    if (edits.insertAfter.has(line)) output++;
  }

  return carried;
}
