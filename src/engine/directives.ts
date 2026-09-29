import { commentIndex, lineAt } from "./doc.ts";
import type { Comment, Doc, StatementList } from "./model.ts";

export type Region = { start: number; end: number };

type Directive = "ignore" | "off" | "on";

function directive(comment: Comment): Directive | null {
  const value = comment.type === "Block" ? comment.value.replace(/^[\s*]*/, "") : comment.value;
  const match = /^stanza-(ignore|off|on)(?![\p{ID_Continue}-])/u.exec(value.trim());
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

function enclosing(doc: Doc, containers: Region[], comment: Comment): Region {
  let block: Region = { start: 0, end: doc.text.length };
  for (const container of containers)
    if (
      container.start < comment.start &&
      comment.end <= container.end &&
      (container.start > block.start ||
        (container.start === block.start && container.end < block.end))
    )
      block = container;

  return block;
}

function nests(sorted: Region[]): boolean {
  const open: Region[] = [];
  for (const container of sorted) {
    while (open.length > 0 && open.at(-1)!.end <= container.start) open.pop();
    if (open.length > 0 && container.end > open.at(-1)!.end) return false;
    open.push(container);
  }

  return true;
}

function enclosingAll(doc: Doc, containers: Region[], marks: Comment[]): Region[] {
  const sorted = containers.toSorted(
    (left, right) => left.start - right.start || right.end - left.end,
  );

  if (!nests(sorted)) return marks.map((comment) => enclosing(doc, containers, comment));

  const whole: Region = { start: 0, end: doc.text.length };
  const open: Region[] = [];

  let next = 0;
  return marks.map((comment) => {
    for (; next < sorted.length && sorted[next]!.start < comment.start; next++) {
      while (open.length > 0 && open.at(-1)!.end <= sorted[next]!.start) open.pop();
      open.push(sorted[next]!);
    }

    while (open.length > 0 && open.at(-1)!.end < comment.end) open.pop();
    return open.at(-1) ?? whole;
  });
}

export function marks(doc: Doc): Comment[] {
  return doc.comments.filter(
    (comment) => directive(comment) === "off" || directive(comment) === "on",
  );
}

export function regions(doc: Doc, marks: Comment[], containers: Region[]): Region[] {
  if (marks.length === 0) return [];

  const ranges = enclosingAll(doc, containers, marks);
  const open = new Map<string, Region & { depth: number }>();
  const result: Region[] = [];
  for (const [index, comment] of marks.entries()) {
    const range = ranges[index]!;
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

  result.sort((left, right) => left.start - right.start);

  const merged: Region[] = [];
  for (const region of result) {
    const previous = merged.at(-1);
    if (previous && region.start <= previous.end) previous.end = Math.max(previous.end, region.end);
    else merged.push(region);
  }

  return merged;
}

export function within(regions: Region[], offset: number): boolean {
  let low = 0;
  let high = regions.length;
  while (low < high) {
    const middle = Math.floor((low + high) / 2);
    if (regions[middle]!.start <= offset) low = middle + 1;
    else high = middle;
  }

  const region = regions[low - 1];
  return region !== undefined && offset < region.end;
}

export function freeze(
  doc: Doc,
  marked: Comment[],
  lists: StatementList[],
  containers: Region[],
): { lists: StatementList[]; frozen: Region[] } {
  if (marked.length === 0) return { lists, frozen: [] };

  const frozen = regions(doc, marked, containers);
  for (const list of lists)
    for (const stmt of list.stmts) if (within(frozen, stmt.start)) stmt.frozen = true;

  return { lists: lists.filter((list) => !within(frozen, list.start)), frozen };
}
