import { blankLines, commentIndex, finding, lineAt } from "./doc.ts";
import { within, type Region } from "./directives.ts";
import type {
  Binding,
  Doc,
  Gap,
  GapDecision,
  JoinRule,
  Kind,
  LineEdits,
  Path,
  StatementList,
  Stmt,
} from "./model.ts";
import type { Finding } from "./types.ts";

const JUMP_KINDS = new Set<Kind>(["return", "throw", "continue", "break"]);

const USE_KINDS = new Set<Kind>(["loop", "try", "function"]);

const BLOCK_KINDS = new Set<Kind>(["if", "loop", "try", "switch"]);

function pathName(path: Path): string {
  return path.reduce((name, segment) =>
    segment.startsWith("[") ? name + segment : `${name}.${segment}`,
  );
}

function readBy(bound: Binding, stmt: Stmt): string | undefined {
  if (bound instanceof Set) return stmt.references(bound)[Symbol.iterator]().next().value;
  return stmt.readsPath(bound) ? pathName(bound) : undefined;
}

function joinRule(prev: Stmt, next: Stmt): JoinRule | null {
  if (next.kind === "if") return "guard-join";
  if (next.kind === "return" || next.kind === "switch") return "consume-join";
  if (USE_KINDS.has(next.kind) && prev.kind === "declaration") return "use-join";

  return null;
}

function declarationJoin(prev: Stmt, next: Stmt): Ruled | null {
  const name = prev.binds === null ? undefined : readBy(prev.binds, next);
  if (name === undefined) return null;

  const rule = joinRule(prev, next);
  if (rule === null || next.word === null) return null;

  return { want: "none", rule, name, reader: next.word };
}

function shortBody(list: StatementList): boolean {
  return (
    list.kind !== "switch" &&
    list.stmts.length >= 2 &&
    list.stmts.length <= 3 &&
    list.stmts.every((stmt) => stmt.compact)
  );
}

export type Ruled = Exclude<GapDecision, { want: "keep" | "frozen" }>;

type Step = (list: StatementList, prev: Stmt, next: Stmt) => Ruled | null;

const LADDER: Step[] = [
  (list) => (shortBody(list) ? { want: "none", rule: "short-body" } : null),
  (_list, prev, next) =>
    prev.guard && next.guard && prev.compact && next.compact
      ? { want: "none", rule: "guard-chain" }
      : null,
  (_list, prev) => (prev.multiline ? { want: "at-least-one", rule: "after-multiline" } : null),
  (list, prev) =>
    list.kind === "switch" && prev.kind === "case" && prev.caseBody
      ? { want: "at-least-one", rule: "switch-clauses" }
      : null,
  (_list, prev, next) => declarationJoin(prev, next),
  (_list, prev, next) =>
    prev.guard?.jump && next.kind !== "if" && !JUMP_KINDS.has(next.kind)
      ? { want: "at-least-one", rule: "after-guard" }
      : null,
];

export function* matches(list: StatementList, prev: Stmt, next: Stmt): Generator<Ruled> {
  for (const step of LADDER) {
    const decision = step(list, prev, next);
    if (decision) yield decision;
  }
}

export function decide(list: StatementList, prev: Stmt, next: Stmt): GapDecision {
  if (prev.frozen || next.frozen) return { want: "frozen" };
  for (const decision of matches(list, prev, next)) return decision;
  return { want: "keep" };
}

function mayJoinRun(gap: Gap): boolean {
  return (
    gap.decision.want === "keep" &&
    !gap.next.detached &&
    gap.prev.declaration?.letLike === true &&
    !gap.prev.multiline &&
    gap.prev.binds !== null
  );
}

function referenceRanks(block: Stmt, bindings: Binding[]): Map<string, number> {
  const names = new Set<string>();
  for (const bound of bindings) if (bound instanceof Set) for (const name of bound) names.add(name);

  const ranks = new Map<string, number>();
  if (names.size === 0) return ranks;

  for (const name of block.references(names)) {
    if (!ranks.has(name)) ranks.set(name, ranks.size);
    if (ranks.size === names.size) break;
  }

  return ranks;
}

function firstRead(bound: Binding, block: Stmt, ranks: Map<string, number>): string | undefined {
  if (!(bound instanceof Set)) return readBy(bound, block);

  let first: string | undefined;
  for (const name of bound) {
    const rank = ranks.get(name);
    if (rank !== undefined && (first === undefined || rank < ranks.get(first)!)) first = name;
  }

  return first;
}

function letSteps(gaps: Gap[]): void {
  for (let index = 1; index < gaps.length; index++) {
    const joined = gaps[index]!;
    const { decision } = joined;
    if (
      !joined.prev.declaration?.letLike ||
      decision.want !== "none" ||
      decision.rule === "short-body"
    )
      continue;

    let start = index;
    while (start > 0 && mayJoinRun(gaps[start - 1]!)) start--;

    const run = gaps.slice(start, index).toReversed();
    const ranks = referenceRanks(
      joined.next,
      run.map((gap) => gap.prev.binds!),
    );

    let first = index;
    for (const gap of run) {
      const name = firstRead(gap.prev.binds!, joined.next, ranks);
      if (name === undefined) break;

      first--;
      gap.decision = "name" in decision ? { ...decision, name } : decision;
    }

    const above = gaps[first - 1];
    if (above?.decision.want === "keep")
      above.decision = { want: "at-least-one", rule: "let-step" };
  }
}

function bracketedTry(prev: Stmt, next: Stmt): boolean {
  return prev.operation !== null && next.finallyRepeats(prev.operation);
}

export function blockSpacing(doc: Doc, gap: Gap): Finding[] {
  const { prev, next, blank, decision } = gap;
  if (
    decision.want !== "keep" ||
    blank !== 0 ||
    prev.multiline ||
    !next.multiline ||
    !BLOCK_KINDS.has(next.kind)
  )
    return [];

  if (prev.kind === "if" && next.kind === "if") return [];
  if (bracketedTry(prev, next)) return [];

  return [finding(doc, next.start, "block-spacing")];
}

function gapEdits(doc: Doc, gap: Gap, edits: LineEdits): Finding[] {
  const { prev, next, blank, decision } = gap;
  if (
    decision.want === "keep" ||
    decision.want === "frozen" ||
    next.detached ||
    next.startLine <= prev.endLine ||
    (decision.want === "none" ? blank === 0 : blank > 0)
  )
    return [];

  if (decision.want === "none")
    for (const line of blankLines(doc, prev.endLine, next.startLine)) edits.deleteLines.add(line);
  else edits.insertAfter.add(next.startLine - 1);

  if ("name" in decision)
    return [finding(doc, next.start, decision.rule, decision.name, decision.reader)];

  return [finding(doc, next.start, decision.rule)];
}

function edgeEdits(
  doc: Doc,
  list: StatementList,
  regions: Region[],
  edits: LineEdits,
  touches: (first: number, last: number) => boolean,
): Finding[] {
  const { openLine, closeLine } = list;
  if (openLine === null || closeLine === null || openLine === closeLine) return [];

  const to = commentIndex(doc, list.end);

  let opened = openLine;
  let from = commentIndex(doc, list.start + 1);
  while (from < to && lineAt(doc, doc.comments[from]!.start) === openLine) {
    opened = lineAt(doc, doc.comments[from]!.end - 1);
    from++;
  }

  const firstComment = from < to ? lineAt(doc, doc.comments[from]!.start) : closeLine;
  const lastComment = from < to ? lineAt(doc, doc.comments[to - 1]!.end - 1) : opened;

  const first = Math.min(list.stmts[0]?.startLine ?? closeLine, firstComment);
  const last = Math.max(list.stmts.at(-1)?.endLine ?? opened, lastComment);

  const findings: Finding[] = [];
  for (const [after, before, side, frozen] of [
    [opened, first, "after {", list.stmts[0]?.frozen],
    [last, closeLine, "before }", list.stmts.at(-1)?.frozen],
  ] as const) {
    if (frozen || !touches(after, before)) continue;

    for (const line of blankLines(doc, after, before)) {
      if (edits.deleteLines.has(line) || within(regions, doc.lineStarts[line - 1]!)) continue;
      edits.deleteLines.add(line);
      findings.push(finding(doc, doc.lineStarts[line - 1]!, "edge-blank", side));
    }
  }

  return findings;
}

function walls(
  doc: Doc,
  list: StatementList,
  gaps: Gap[],
  touches: (first: number, last: number) => boolean,
): Finding[] {
  const findings: Finding[] = [];
  if (list.kind === "switch") return findings;

  let runStart: Stmt | undefined;
  let length = 0;
  for (let index = 0; index <= list.stmts.length; index++) {
    const stmt = list.stmts[index];
    const gap = gaps[index - 1];
    const separated =
      gap &&
      (touches(gap.prev.startLine, gap.next.endLine)
        ? gap.decision.want === "frozen" ||
          gap.decision.want === "at-least-one" ||
          (gap.decision.want === "keep" && gap.blank > 0)
        : gap.decision.want === "frozen" || gap.blank > 0);

    if (!stmt || stmt.multiline || separated) {
      if (runStart && length >= 6 && touches(runStart.startLine, list.stmts[index - 1]!.endLine))
        findings.push(finding(doc, runStart.start, "wall"));

      runStart = undefined;
      length = 0;
    }

    if (!stmt || stmt.multiline) continue;

    runStart ??= stmt;
    length++;
  }

  return findings;
}

export function listGaps(doc: Doc, list: StatementList): Gap[] {
  const gaps: Gap[] = [];
  for (let index = 1; index < list.stmts.length; index++) {
    const prev = list.stmts[index - 1]!;
    const next = list.stmts[index]!;
    gaps.push({
      prev,
      next,
      blank: blankLines(doc, prev.endLine, next.startLine).length,
      decision: decide(list, prev, next),
    });
  }

  letSteps(gaps);
  return gaps;
}

export function spacing(
  doc: Doc,
  lists: StatementList[],
  regions: Region[],
  touches: (first: number, last: number) => boolean = () => true,
): { edits: LineEdits; findings: Finding[] } {
  const edits: LineEdits = { deleteLines: new Set(), insertAfter: new Set() };
  const findings: Finding[] = [];
  for (const list of lists) {
    const gaps = listGaps(doc, list);
    for (const gap of gaps)
      if (touches(gap.prev.startLine, gap.next.endLine))
        findings.push(...gapEdits(doc, gap, edits), ...blockSpacing(doc, gap));

    findings.push(
      ...edgeEdits(doc, list, regions, edits, touches),
      ...walls(doc, list, gaps, touches),
    );
  }

  return { edits, findings };
}
