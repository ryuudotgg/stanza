import { lineAt } from "./doc.ts";
import { lowerBound, offsetMap, type OffsetEdit } from "./edits.ts";
import type { Changed, Doc, LineEdits } from "./model.ts";

export function touching(changed: Changed | undefined): (first: number, last: number) => boolean {
  if (changed === undefined) return () => true;

  const lines = [...changed.lines].sort((left, right) => left - right);
  const deletedAfter = [...changed.deletedAfter].sort((left, right) => left - right);
  return (first, last) =>
    (lines[lowerBound(lines, first)] ?? Infinity) <= last ||
    (deletedAfter[lowerBound(deletedAfter, first)] ?? Infinity) < last;
}

const braces = (line: string) => line.replace(/[^{}]/g, "");
const braceOnly = (line: string) => /^[\s{}]*$/.test(line) && braces(line) !== "";

function literals(line: string): { code: string; text: string } {
  let code = "";
  let text = "";
  for (let index = 0; index < line.length; index++) {
    const char = line[index]!;
    const closes = line[index + 1] === "*" ? line.indexOf("*/", index + 2) : -1;
    if (char === "/" && closes >= 0) {
      text += `${line.slice(index, closes + 2)}\u0000`;
      code += " ";
      index = closes + 1;
      continue;
    }

    if (char === "/" && (line[index + 1] === "/" || line[index + 1] === "*"))
      return { code: `${code}${char}${line[index + 1]}`, text: text + line.slice(index + 2) };

    code += char;
    if (char !== '"' && char !== "'" && char !== "`") continue;

    let end = index + 1;
    while (end < line.length && line[end] !== char) end += line[end] === "\\" ? 2 : 1;

    text += `${line.slice(index + 1, end)}\u0000`;
    code += line[end] ?? "";
    index = end;
  }

  return { code, text };
}

function unbraced(line: string): string {
  const { code, text } = literals(line.replace(/\r$/, ""));
  return `${unbracedCode(code)}\u0000${text}`;
}

interface Piece {
  indent: string;
  code: string;
  text: string;
  opens: boolean;
}

function piece(line: string): Piece {
  const { code, text } = literals(line.replace(/\r$/, ""));
  const unbracedLine = unbracedCode(code);
  return {
    indent: /^\s*/.exec(code)![0],
    code: unbracedLine.trim().replace(/\s+/g, " "),
    text,
    opens: /(\)|\belse|\bdo)$/.test(unbracedLine),
  };
}

const joinKey = (indent: string, code: string, text: string) => `${indent}${code}\u0000${text}`;

function unbracedCode(line: string): string {
  return line
    .replace(/^(\s*)\}\s*/, "$1")
    .replace(/(\)|\belse|\bdo)\s*\{(?=\s*(?:\/\/|\/\*|$)|.*\}\s*$)\s*/g, "$1 ")
    .replace(/\s*\}\s*(?=else\b|while\b|\/\/|\/\*|\}|$)/g, " ")
    .trimEnd();
}

const editLimit = 2000;
function shortestEdit(before: number[], after: number[]): [number, number][] | undefined {
  const offset = editLimit + 1;
  const reach = new Int32Array(2 * offset + 1);
  const trace: Int32Array[] = [];
  for (let edits = 0; edits <= editLimit; edits++) {
    for (let diagonal = -edits; diagonal <= edits; diagonal += 2) {
      const down =
        diagonal === -edits ||
        (diagonal !== edits && reach[offset + diagonal - 1]! < reach[offset + diagonal + 1]!);

      let x = down ? reach[offset + diagonal + 1]! : reach[offset + diagonal - 1]! + 1;
      let y = x - diagonal;
      while (x < before.length && y < after.length && before[x] === after[y]) {
        x++;
        y++;
      }

      reach[offset + diagonal] = x;
      if (x < before.length || y < after.length) continue;

      trace.push(reach.slice(offset - edits, offset + edits + 1));
      return backtrack(trace, before.length, after.length);
    }

    trace.push(reach.slice(offset - edits, offset + edits + 1));
  }

  return undefined;
}

function matches(before: number[], after: number[]): [number, number][] | undefined {
  let head = 0;
  while (head < before.length && head < after.length && before[head] === after[head]) head++;

  let tail = 0;
  while (
    tail < before.length - head &&
    tail < after.length - head &&
    before[before.length - 1 - tail] === after[after.length - 1 - tail]
  )
    tail++;

  const middle = shortestEdit(
    before.slice(head, before.length - tail),
    after.slice(head, after.length - tail),
  );

  if (!middle) return undefined;

  return [
    ...Array.from({ length: head }, (_, index): [number, number] => [index, index]),
    ...middle.map(([x, y]): [number, number] => [x + head, y + head]),
    ...Array.from({ length: tail }, (_, index): [number, number] => [
      before.length - tail + index,
      after.length - tail + index,
    ]),
  ];
}

function backtrack(trace: Int32Array[], width: number, height: number): [number, number][] {
  const pairs: [number, number][] = [];

  let x = width;
  let y = height;
  for (let edits = trace.length - 1; edits >= 0; edits--) {
    const diagonal = x - y;

    let startX = 0;
    let startY = 0;
    if (edits > 0) {
      const previous = trace[edits - 1]!;
      const at = (index: number) => previous[index + edits - 1]!;
      const from =
        diagonal === -edits || (diagonal !== edits && at(diagonal - 1) < at(diagonal + 1))
          ? diagonal + 1
          : diagonal - 1;

      startX = at(from);
      startY = startX - from;
    }

    const snakeX = edits > 0 && startX - startY === diagonal - 1 ? startX + 1 : startX;
    const snakeY = edits > 0 && startX - startY === diagonal + 1 ? startY + 1 : startY;
    while (x > snakeX && y > snakeY) pairs.push([--x, --y]);

    x = startX;
    y = startY;
  }

  return pairs.reverse();
}

interface Lines {
  before: string[];
  after: { text: string; line: number }[];
  at: number;
}

function sides(section: string[]): { file: Lines; blocks: Lines[] } {
  const file: Lines = { before: [], after: [], at: 0 };
  const blocks: Lines[] = [];

  let line = 0;
  let beforeLeft = 0;
  let afterLeft = 0;
  let block: Lines | undefined;
  for (const row of section) {
    const header = /^@@ -\d+(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(row);
    if (header) {
      beforeLeft = header[1] === undefined ? 1 : Number(header[1]);
      afterLeft = header[3] === undefined ? 1 : Number(header[3]);
      line = afterLeft === 0 ? Number(header[2]) : Number(header[2]) - 1;
      block = undefined;
      continue;
    }

    if (beforeLeft === 0 && afterLeft === 0) continue;

    const text = row.slice(1);
    if (row.startsWith(" ")) {
      beforeLeft--;
      afterLeft--;

      file.before.push(text);
      file.after.push({ text, line: ++line });
      block = undefined;
      continue;
    }

    if (!row.startsWith("-") && !row.startsWith("+")) continue;
    if (!block) blocks.push((block = { before: [], after: [], at: line }));
    if (row.startsWith("+")) {
      afterLeft--;
      file.after.push({ text, line: ++line });
      block.after.push({ text, line });
    } else {
      beforeLeft--;
      if (braceOnly(text)) continue;

      file.before.push(text);
      block.before.push(text);
    }
  }

  file.at = (file.after[0]?.line ?? 1) - 1;
  return { file, blocks };
}

interface Group {
  first: number;
  last: number;
  text: string;
}

const joinDepth = 8;
function rejoin(before: string[], from: number, to: number, after: Lines["after"]): Group[] {
  const joins = new Map<string, string>();
  for (const { text } of after) {
    const { indent, code, text: literal } = piece(text);
    joins.set(joinKey(indent, code, literal), text);
  }

  const groups: Group[] = [];
  for (let index = from; index < to; index++) {
    const first = piece(before[index]!);

    let last = index;
    let code = first.code;
    let text = first.text;
    let found: string | undefined;
    if (first.opens && !joins.has(joinKey(first.indent, code, text)))
      for (let next = index + 1; next < Math.min(to, index + joinDepth); next++) {
        const part = piece(before[next]!);
        if (part.code !== "") code = `${code} ${part.code}`;
        text += part.text;

        found = joins.get(joinKey(first.indent, code, text));
        if (found !== undefined) {
          last = next;
          break;
        }

        if (!part.opens) break;
      }

    groups.push({ first: index, last, text: found ?? before[index]! });
    index = last;
  }

  return groups;
}

interface Pair extends Group {
  y: number;
}

function classify(
  { before, after, at }: Lines,
  changed: { lines: Set<number>; deletedAfter: Set<number> },
): boolean {
  const ids = new Map<string, number>();
  const id = (text: string) => {
    const key = braceOnly(text) ? text : unbraced(text);

    let value = ids.get(key);
    if (value === undefined) ids.set(key, (value = ids.size));
    return value;
  };

  const pairs = matches(
    before.map(id),
    after.map(({ text }) => id(text)),
  );

  if (!pairs) return false;

  const paired: Pair[] = [];

  let x = 0;
  let y = 0;
  for (const [pairX, pairY] of [...pairs, [before.length, after.length] as const]) {
    const groups = x < pairX && y < pairY ? rejoin(before, x, pairX, after.slice(y, pairY)) : [];
    if (groups.length < pairX - x)
      for (const [groupX, groupY] of matches(
        groups.map(({ text }) => id(text)),
        after.slice(y, pairY).map(({ text }) => id(text)),
      ) ?? [])
        paired.push({ ...groups[groupX]!, y: y + groupY });

    if (pairX < before.length)
      paired.push({ first: pairX, last: pairX, text: before[pairX]!, y: pairY });

    x = pairX + 1;
    y = pairY + 1;
  }

  x = 0;
  y = 0;
  paired.push({ first: before.length, last: before.length, text: "", y: after.length });

  for (const pair of paired) {
    if (x < pair.first) changed.deletedAfter.add(at);

    for (; y < pair.y; y++) changed.lines.add((at = after[y]!.line));
    if (pair.first === before.length) break;

    const now = after[pair.y]!;
    if (pair.text !== now.text && braces(now.text).length >= braces(pair.text).length)
      changed.lines.add(now.line);

    at = now.line;
    x = pair.last + 1;
    y = pair.y + 1;
  }

  return true;
}

export function diffChanges(section: string[]): Changed {
  const { file, blocks } = sides(section);
  const changed = { lines: new Set<number>(), deletedAfter: new Set<number>() };
  if (!classify(file, changed))
    for (const block of blocks) {
      if (classify(block, changed)) continue;
      if (block.before.length > 0) changed.deletedAfter.add(block.at);
      for (const { line } of block.after) changed.lines.add(line);
    }

  for (const line of changed.deletedAfter)
    if (changed.lines.has(line) || changed.lines.has(line + 1)) changed.deletedAfter.delete(line);

  return changed;
}

function lineBefore(next: Doc, offset: number): number {
  const line = lineAt(next, Math.min(offset, next.text.length));
  return next.lineStarts[line - 1] === offset ? line - 1 : line;
}

export function afterOffsets(
  doc: Doc,
  changed: Changed | undefined,
  edits: OffsetEdit[],
  next: Doc,
): Changed | undefined {
  if (changed === undefined) return undefined;

  const shift = offsetMap(edits).forward;
  const lines = new Set<number>();
  const deletedAfter = new Set<number>();
  for (const line of changed.lines) {
    const from = doc.lineStarts[line - 1];
    if (from === undefined) continue;

    const start = shift(from);
    const end = shift(doc.lineStarts[line] ?? doc.text.length);
    if (start === end) {
      deletedAfter.add(lineBefore(next, start));
      continue;
    }

    const last = lineAt(next, Math.min(end, next.text.length) - 1);
    for (let number = lineAt(next, start); number <= last; number++) lines.add(number);
  }

  for (const line of changed.deletedAfter)
    deletedAfter.add(lineBefore(next, shift(doc.lineStarts[line] ?? doc.text.length)));

  return { lines, deletedAfter };
}

export function afterLines(
  doc: Doc,
  changed: Changed | undefined,
  edits: LineEdits,
): Changed | undefined {
  if (changed === undefined) return undefined;

  const lines = new Set<number>();
  const deletedAfter = new Set<number>();
  if (changed.deletedAfter.has(0)) deletedAfter.add(0);

  let output = 0;
  for (let line = 1; line <= doc.lines.length; line++) {
    if (!edits.deleteLines.has(line)) {
      output++;
      if (changed.lines.has(line)) lines.add(output);
    } else if (changed.lines.has(line)) deletedAfter.add(output);

    if (changed.deletedAfter.has(line)) deletedAfter.add(output);
    if (edits.insertAfter.has(line)) output++;
  }

  return { lines, deletedAfter };
}
