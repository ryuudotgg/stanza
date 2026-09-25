import type { Node, Statement } from "oxc-parser";
import { BLOCK_TYPES, boundNames, children, references } from "./ast.ts";
import { blankLines, commentIndex, finding, lineAt, source } from "./doc.ts";
import type { List } from "./lists.ts";
import type { Doc, Gap, GapDecision, LineEdits, StatementList, Stmt } from "./model.ts";
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

const JUMP_TYPES = new Set([
  "ReturnStatement",
  "ThrowStatement",
  "ContinueStatement",
  "BreakStatement",
]);

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

function boundBy(doc: Doc, node: Statement): Set<string> | string | null {
  if (node.type === "VariableDeclaration") return boundNames(node);
  if (node.type !== "ExpressionStatement" || node.expression.type !== "AssignmentExpression")
    return null;

  const target = node.expression.left;
  return target.type === "MemberExpression" ? source(doc, target) : boundNames(target);
}

function related(doc: Doc, bound: Set<string> | string, node: Node): boolean {
  return typeof bound === "string" ? source(doc, node).includes(bound) : references(node, bound);
}

function declarationJoin(doc: Doc, prev: Stmt, next: Stmt): GapDecision {
  if (prev.node.type === "SwitchCase") return { want: "keep" };

  const bound = boundBy(doc, prev.node);
  if (bound === null || !related(doc, bound, next.node)) return { want: "keep" };

  const node = next.node;
  if (node.type === "IfStatement") return { want: "none", rule: "guard-join" };
  if (node.type === "ReturnStatement" || node.type === "SwitchStatement")
    return { want: "none", rule: "consume-join" };
  if (USE_TYPES.has(node.type) && prev.node.type === "VariableDeclaration")
    return { want: "none", rule: "use-join" };

  return { want: "keep" };
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
    related(doc, boundNames(gap.prev.node), block.node)
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
      gaps[first]!.decision = decision;
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
  if (node.type === "ExpressionStatement" && operation(doc, node) === expected) return true;
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

  return [
    finding(
      doc,
      next.node.start,
      "block-spacing",
      "blank line expected before this block, or join it to the step above",
      false,
    ),
  ];
}

function gapMessage(decision: Exclude<GapDecision, { want: "keep" }>): string {
  switch (decision.rule) {
    case "after-multiline":
      return "blank line expected after the multi-line statement above";

    case "switch-clauses":
      return "blank line expected between switch clauses";

    case "short-body":
      return "no blank lines between 2 or 3 single-line statements";

    case "guard-chain":
      return "no blank line between consecutive single-line guards";

    case "let-step":
      return "blank line expected above a let that the block below assigns or uses";

    case "after-guard":
      return "blank line expected after the guard above, the next statement starts a new step";

    default:
      return "join this to the line above (remove the blank line)";
  }
}

function gapEdits(doc: Doc, gap: Gap, edits: LineEdits): Finding[] {
  const { prev, next, blank, decision } = gap;
  if (
    decision.want === "keep" ||
    next.detached ||
    next.startLine <= prev.endLine ||
    (decision.want === "none" ? blank === 0 : blank > 0)
  )
    return [];

  if (decision.want === "none")
    for (const line of blankLines(doc, prev.endLine, next.startLine)) edits.deleteLines.add(line);
  else edits.insertAfter.add(next.startLine - 1);

  return [finding(doc, next.node.start, decision.rule, gapMessage(decision), true)];
}

function edgeEdits(doc: Doc, list: List, edits: LineEdits): Finding[] {
  const { openLine, closeLine } = list;
  if (openLine === null || closeLine === null || openLine === closeLine) return [];

  const from = commentIndex(doc, list.start + 1);
  const to = commentIndex(doc, list.end);

  const firstComment = from < to ? lineAt(doc, doc.comments[from]!.start) : closeLine;
  const lastComment = from < to ? lineAt(doc, doc.comments[to - 1]!.end - 1) : openLine;

  const first = Math.min(list.stmts[0]?.startLine ?? closeLine, firstComment);
  const last = Math.max(list.stmts.at(-1)?.endLine ?? openLine, lastComment);

  const findings: Finding[] = [];
  for (const [after, before, message] of [
    [openLine, first, "no blank line right after {"],
    [last, closeLine, "no blank line right before }"],
  ] as const)
    for (const line of blankLines(doc, after, before)) {
      if (edits.deleteLines.has(line)) continue;
      edits.deleteLines.add(line);
      findings.push(finding(doc, doc.lineStarts[line - 1]!, "edge-blank", message, true));
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
      (gap.decision.want === "at-least-one" || (gap.decision.want === "keep" && gap.blank > 0));

    if (stmt.multiline || separated) {
      runStart = undefined;
      length = 0;
    }

    if (stmt.multiline) continue;

    runStart ??= stmt;
    length++;

    if (length === 6)
      findings.push(
        finding(
          doc,
          runStart.node.start,
          "wall",
          "6 or more statements with no blank line between them; separate the steps",
          false,
        ),
      );
  }

  return findings;
}

export function spacing(doc: Doc, lists: List[]): { edits: LineEdits; findings: Finding[] } {
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

    findings.push(...edgeEdits(doc, list, edits), ...walls(doc, list, gaps));
  }

  return { edits, findings };
}
