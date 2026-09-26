import type { Comment } from "oxc-parser";
import { commentIndex, lineAt } from "./doc.ts";
import type { List } from "./lists.ts";
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

function enclosing(doc: Doc, lists: List[], comment: Comment): Region {
  let best: Region = { start: 0, end: doc.text.length };
  for (const list of lists)
    if (
      list.start < comment.start &&
      comment.end <= list.end &&
      list.end - list.start < best.end - best.start
    )
      best = list;

  return best;
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

export function regions(doc: Doc, lists: List[]): Region[] {
  const marks = doc.comments
    .filter((comment) => directive(comment) === "off" || directive(comment) === "on")
    .map((comment) => ({
      comment,
      kind: directive(comment),
      range: enclosing(doc, lists, comment),
    }));

  return marks
    .filter((mark) => mark.kind === "off")
    .map(({ comment, range }) => {
      const on = marks.find(
        (mark) =>
          mark.kind === "on" &&
          mark.comment.start > comment.start &&
          mark.range.start === range.start &&
          mark.range.end === range.end,
      );

      return { start: comment.start, end: on?.comment.start ?? range.end };
    });
}

export function within(regions: Region[], offset: number): boolean {
  return regions.some((region) => region.start <= offset && offset < region.end);
}
