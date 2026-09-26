import type { Node, Statement } from "oxc-parser";
import { BLOCK_TYPES, boundNames, children, firstReference } from "./ast.ts";
import { blankLines, commentIndex, finding, lineAt, source } from "./doc.ts";
import { within, type Region } from "./directives.ts";
import type { List } from "./lists.ts";
import type { Doc, Gap, GapDecision, JoinRule, LineEdits, StatementList, Stmt } from "./model.ts";
import type { Finding } from "./types.ts";

const USE_TYPES = new Set([
  "ForStatement",
  "ForInStatement",
  "ForOfStatement",
  "WhileStatement",
  "DoWhileStatement",
  "TryStatement",
  "FunctionDeclaration",
]);

const READERS: Record<string, string> = {
  IfStatement: "if",
  ReturnStatement: "return",
  SwitchStatement: "switch",
  ForStatement: "for",
  ForInStatement: "for",
  ForOfStatement: "for",
  WhileStatement: "while",
  DoWhileStatement: "do",
  TryStatement: "try",
  FunctionDeclaration: "function",
};

const JUMP_TYPES = new Set([
  "ReturnStatement",
  "ThrowStatement",
  "ContinueStatement",
  "BreakStatement",
]);

function unwrapPath(node: Node): Node {
  if (
    node.type === "ChainExpression" ||
    node.type === "TSNonNullExpression" ||
    node.type === "TSAsExpression" ||
    node.type === "TSSatisfiesExpression" ||
    node.type === "TSTypeAssertion" ||
    node.type === "ParenthesizedExpression"
  )
    return unwrapPath(node.expression);

  return node;
}

function memberPath(doc: Doc, node: Node): string[] | undefined {
  const unwrapped = unwrapPath(node);
  if (unwrapped.type === "Identifier") return [unwrapped.name];
  if (unwrapped.type === "ThisExpression") return ["this"];
  if (unwrapped.type === "Super") return ["super"];
  if (unwrapped.type !== "MemberExpression") return undefined;

  const path = memberPath(doc, unwrapped.object);
  if (!path) return undefined;

  const { computed, property } = unwrapped;
  if (computed) return [...path, `[${source(doc, property)}]`];
  if (property.type === "PrivateIdentifier") return [...path, `#${property.name}`];

  return [...path, property.name];
}

function pathName(path: string[]): string {
  return path.reduce((name, segment) =>
    segment.startsWith("[") ? name + segment : `${name}.${segment}`,
  );
}

function readsPath(doc: Doc, node: Node, path: string[], root = true): boolean {
  if (node.type === "FunctionExpression" || node.type === "ArrowFunctionExpression") return false;
  if (node.type === "FunctionDeclaration" && !root) return false;

  const candidate = node.type === "MemberExpression" ? memberPath(doc, node) : undefined;
  if (candidate?.length === path.length && candidate.every((segment, i) => segment === path[i]))
    return true;

  return children(node).some(([, child]) => readsPath(doc, child, path, false));
}

function isGuard(node: Node): boolean {
  if (node.type !== "IfStatement" || node.alternate) return false;
  return node.consequent.type !== "BlockStatement" || node.consequent.body.length === 1;
}

function jumpGuard(node: Node): boolean {
  if (!isGuard(node) || node.type !== "IfStatement") return false;

  const body =
    node.consequent.type === "BlockStatement" ? node.consequent.body[0] : node.consequent;

  return body !== undefined && JUMP_TYPES.has(body.type);
}

function boundBy(doc: Doc, node: Statement): Set<string> | string[] | null {
  if (node.type === "VariableDeclaration") return boundNames(node);
  if (node.type !== "ExpressionStatement" || node.expression.type !== "AssignmentExpression")
    return null;

  const target = node.expression.left;
  return unwrapPath(target).type === "MemberExpression"
    ? (memberPath(doc, target) ?? null)
    : boundNames(target);
}

function readBy(doc: Doc, bound: Set<string> | string[], node: Node): string | undefined {
  if (bound instanceof Set) return firstReference(node, bound);
  return readsPath(doc, node, bound) ? pathName(bound) : undefined;
}

function joinRule(prev: Stmt, node: Node): JoinRule | null {
  if (node.type === "IfStatement") return "guard-join";
  if (node.type === "ReturnStatement" || node.type === "SwitchStatement") return "consume-join";
  if (USE_TYPES.has(node.type) && prev.node.type === "VariableDeclaration") return "use-join";

  return null;
}

function declarationJoin(doc: Doc, prev: Stmt, next: Stmt): GapDecision {
  if (prev.node.type === "SwitchCase") return { want: "keep" };

  const bound = boundBy(doc, prev.node);
  const name = bound === null ? undefined : readBy(doc, bound, next.node);
  if (name === undefined) return { want: "keep" };

  const rule = joinRule(prev, next.node);
  if (rule === null) return { want: "keep" };

  return { want: "none", rule, name, reader: READERS[next.node.type]! };
}

function compact(doc: Doc, node: Node): boolean {
  if (lineAt(doc, node.start) === lineAt(doc, node.end - 1)) return true;

  const body =
    node.type === "IfStatement" && !node.alternate
      ? node.consequent
      : node.type === "ForStatement" ||
          node.type === "ForInStatement" ||
          node.type === "ForOfStatement" ||
          node.type === "WhileStatement"
        ? node.body
        : null;

  if (!body || body.type === "BlockStatement") return false;

  return !/[\r\n]/.test(doc.text.slice(node.start, body.start).trimEnd()) && compact(doc, body);
}

function shortBody(doc: Doc, list: StatementList): boolean {
  return (
    list.kind !== "switch" &&
    list.stmts.length >= 2 &&
    list.stmts.length <= 3 &&
    list.stmts.every((stmt) => compact(doc, stmt.node))
  );
}

function decide(doc: Doc, list: StatementList, prev: Stmt, next: Stmt): GapDecision {
  if (prev.frozen || next.frozen) return { want: "frozen" };
  if (shortBody(doc, list)) return { want: "none", rule: "short-body" };
  if (
    isGuard(prev.node) &&
    isGuard(next.node) &&
    compact(doc, prev.node) &&
    compact(doc, next.node)
  )
    return { want: "none", rule: "guard-chain" };

  if (prev.multiline) return { want: "at-least-one", rule: "after-multiline" };
  if (list.kind === "switch")
    return prev.node.type === "SwitchCase" && prev.node.consequent.length > 0
      ? { want: "at-least-one", rule: "switch-clauses" }
      : { want: "keep" };

  const joined = declarationJoin(doc, prev, next);
  if (joined.want !== "keep") return joined;
  if (jumpGuard(prev.node) && next.node.type !== "IfStatement" && !JUMP_TYPES.has(next.node.type))
    return { want: "at-least-one", rule: "after-guard" };

  return { want: "keep" };
}

function isLet(stmt: Stmt): boolean {
  return stmt.node.type === "VariableDeclaration" && stmt.node.kind === "let";
}

function joinsRun(doc: Doc, gap: Gap, block: Stmt): boolean {
  return (
    gap.decision.want === "keep" &&
    !gap.next.detached &&
    isLet(gap.prev) &&
    !gap.prev.multiline &&
    readBy(doc, boundNames(gap.prev.node), block.node) !== undefined
  );
}

function letSteps(doc: Doc, gaps: Gap[]): void {
  for (let index = 1; index < gaps.length; index++) {
    const joined = gaps[index]!;
    const { decision } = joined;
    if (!isLet(joined.prev) || decision.want !== "none" || decision.rule === "short-body") continue;

    let first = index;
    while (first > 0 && joinsRun(doc, gaps[first - 1]!, joined.next)) {
      first--;

      const gap = gaps[first]!;
      gap.decision =
        "name" in decision
          ? { ...decision, name: firstReference(joined.next.node, boundNames(gap.prev.node))! }
          : decision;
    }

    const above = gaps[first - 1];
    if (above?.decision.want === "keep")
      above.decision = { want: "at-least-one", rule: "let-step" };
  }
}

function operation(doc: Doc, node: Statement): string | null {
  if (node.type !== "ExpressionStatement") return null;

  const expression = node.expression;
  if (expression.type === "CallExpression") return `call:${source(doc, expression.callee)}`;
  if (expression.type === "AssignmentExpression") return `assign:${source(doc, expression.left)}`;

  return null;
}

function repeatsOperation(doc: Doc, node: Node, expected: string): boolean {
  if (
    node.type === "FunctionExpression" ||
    node.type === "ArrowFunctionExpression" ||
    node.type === "FunctionDeclaration"
  )
    return false;

  if (node.type === "ExpressionStatement") return operation(doc, node) === expected;
  return children(node).some(([, child]) => repeatsOperation(doc, child, expected));
}

function bracketedTry(doc: Doc, prev: Stmt, next: Stmt): boolean {
  if (prev.node.type === "SwitchCase" || next.node.type !== "TryStatement" || !next.node.finalizer)
    return false;
  const expected = operation(doc, prev.node);
  return expected !== null && repeatsOperation(doc, next.node.finalizer, expected);
}

function blockSpacing(doc: Doc, gap: Gap): Finding[] {
  const { prev, next, blank, decision } = gap;
  if (
    decision.want !== "keep" ||
    blank !== 0 ||
    prev.multiline ||
    !next.multiline ||
    !BLOCK_TYPES.has(next.node.type)
  )
    return [];

  if (prev.node.type === "IfStatement" && next.node.type === "IfStatement") return [];
  if (bracketedTry(doc, prev, next)) return [];

  return [finding(doc, next.node.start, "block-spacing")];
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
    return [finding(doc, next.node.start, decision.rule, decision.name, decision.reader)];

  return [finding(doc, next.node.start, decision.rule)];
}

function edgeEdits(doc: Doc, list: List, regions: Region[], edits: LineEdits): Finding[] {
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
    if (frozen) continue;

    for (const line of blankLines(doc, after, before)) {
      if (edits.deleteLines.has(line) || within(regions, doc.lineStarts[line - 1]!)) continue;
      edits.deleteLines.add(line);
      findings.push(finding(doc, doc.lineStarts[line - 1]!, "edge-blank", side));
    }
  }

  return findings;
}

function walls(doc: Doc, list: StatementList, gaps: Gap[]): Finding[] {
  const findings: Finding[] = [];
  if (list.kind === "switch") return findings;

  let runStart: Stmt | undefined;
  let length = 0;
  for (let index = 0; index < list.stmts.length; index++) {
    const stmt = list.stmts[index]!;
    const gap = gaps[index - 1];
    const separated =
      gap &&
      (gap.decision.want === "frozen" ||
        gap.decision.want === "at-least-one" ||
        (gap.decision.want === "keep" && gap.blank > 0));

    if (stmt.multiline || separated) {
      runStart = undefined;
      length = 0;
    }

    if (stmt.multiline) continue;

    runStart ??= stmt;
    length++;

    if (length === 6) findings.push(finding(doc, runStart.node.start, "wall"));
  }

  return findings;
}

export function spacing(
  doc: Doc,
  lists: List[],
  regions: Region[],
): { edits: LineEdits; findings: Finding[] } {
  const edits: LineEdits = { deleteLines: new Set(), insertAfter: new Set() };
  const findings: Finding[] = [];
  for (const list of lists) {
    const gaps: Gap[] = [];
    for (let index = 1; index < list.stmts.length; index++) {
      const prev = list.stmts[index - 1]!;
      const next = list.stmts[index]!;
      const gap = {
        prev,
        next,
        blank: blankLines(doc, prev.endLine, next.startLine).length,
        decision: decide(doc, list, prev, next),
      };

      gaps.push(gap);
    }

    letSteps(doc, gaps);
    for (const gap of gaps) findings.push(...gapEdits(doc, gap, edits), ...blockSpacing(doc, gap));

    findings.push(...edgeEdits(doc, list, regions, edits), ...walls(doc, list, gaps));
  }

  return { edits, findings };
}
