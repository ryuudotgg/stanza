import type { Node, Statement, SwitchCase } from "oxc-parser";
import { boundNames, children, declared, firstReference } from "./ast.ts";
import { lineAt, source } from "./doc.ts";
import type { Binding, Doc, Item, Kind, Path } from "./model.ts";

const KINDS: Record<string, Kind> = {
  VariableDeclaration: "declaration",
  IfStatement: "if",
  ForStatement: "loop",
  ForInStatement: "loop",
  ForOfStatement: "loop",
  WhileStatement: "loop",
  DoWhileStatement: "loop",
  SwitchStatement: "switch",
  SwitchCase: "case",
  TryStatement: "try",
  FunctionDeclaration: "function",
  ReturnStatement: "return",
  ThrowStatement: "throw",
  BreakStatement: "break",
  ContinueStatement: "continue",
};

const WORDS: Record<string, string> = {
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

const JUMPS: Record<string, string> = {
  ReturnStatement: "return",
  ThrowStatement: "throw",
  ContinueStatement: "continue",
  BreakStatement: "break",
};

function kindOf(node: Statement | SwitchCase): Kind {
  if (node.type === "ExpressionStatement")
    return node.expression.type === "AssignmentExpression" ? "assignment" : "expression";
  return KINDS[node.type] ?? "other";
}

function unwrapPath(node: Node): Node {
  let current = node;
  while (
    current.type === "ChainExpression" ||
    current.type === "TSNonNullExpression" ||
    current.type === "TSAsExpression" ||
    current.type === "TSSatisfiesExpression" ||
    current.type === "TSTypeAssertion" ||
    current.type === "ParenthesizedExpression"
  )
    current = current.expression;

  return current;
}

type Member = Node & { type: "MemberExpression" };

function spine(outer: Member): { members: Member[]; base: Node } {
  const members = [outer];

  let base = unwrapPath(outer.object);
  while (base.type === "MemberExpression") {
    members.push(base);
    base = unwrapPath(base.object);
  }

  return { members, base };
}

function memberSegment(doc: Doc, member: Member): string {
  const { computed, property } = member;
  if (computed) return `[${source(doc, property)}]`;
  if (property.type === "PrivateIdentifier") return `#${property.name}`;
  return property.name;
}

function headName(base: Node): string | undefined {
  if (base.type === "Identifier") return base.name;
  if (base.type === "ThisExpression") return "this";
  if (base.type === "Super") return "super";
  return undefined;
}

function memberPath(doc: Doc, outer: Member): Path | undefined {
  const { members, base } = spine(outer);
  const head = headName(base);
  if (head === undefined) return undefined;

  return [head, ...members.map((member) => memberSegment(doc, member)).reverse()];
}

function spineReads(doc: Doc, members: Member[], base: Node, path: Path): boolean {
  const first = members.length - (path.length - 1);
  if (first < 0 || headName(base) !== path[0]) return false;

  return members
    .slice(first)
    .every((member, index) => memberSegment(doc, member) === path[path.length - 1 - index]);
}

function readsPath(doc: Doc, root: Node, path: Path): boolean {
  const stack = [root];
  while (stack.length > 0) {
    const node = stack.pop()!;
    if (node.type === "FunctionExpression" || node.type === "ArrowFunctionExpression") continue;
    if (node.type === "FunctionDeclaration" && node !== root) continue;
    if (path[0] !== "this" && path[0] !== "super" && declared(node).includes(path[0])) {
      if (node.type === "SwitchStatement") stack.push(node.discriminant);
      continue;
    }

    if (node.type !== "MemberExpression") {
      for (const [, child] of children(node).toReversed()) stack.push(child);
      continue;
    }

    const { members, base } = spine(node);
    if (spineReads(doc, members, base, path)) return true;

    stack.push(base);
    for (const member of members) if (member.computed) stack.push(member.property);
  }

  return false;
}

function guardOf(node: Statement | SwitchCase): { jump: string | null } | null {
  if (node.type !== "IfStatement" || node.alternate) return null;

  const { consequent } = node;
  if (consequent.type !== "BlockStatement") return { jump: JUMPS[consequent.type] ?? null };
  if (consequent.body.length !== 1) return null;

  return { jump: JUMPS[consequent.body[0]!.type] ?? null };
}

function bindsOf(doc: Doc, node: Statement | SwitchCase): Binding | null {
  if (node.type === "VariableDeclaration") return boundNames(node);
  if (node.type !== "ExpressionStatement" || node.expression.type !== "AssignmentExpression")
    return null;

  const target = node.expression.left;
  const member = unwrapPath(target);
  return member.type === "MemberExpression"
    ? (memberPath(doc, member) ?? null)
    : boundNames(target);
}

function compact(doc: Doc, node: Node): boolean {
  let current = node;
  while (true) {
    if (lineAt(doc, current.start) === lineAt(doc, current.end - 1)) return true;

    const body =
      current.type === "IfStatement" && !current.alternate
        ? current.consequent
        : current.type === "ForStatement" ||
            current.type === "ForInStatement" ||
            current.type === "ForOfStatement" ||
            current.type === "WhileStatement"
          ? current.body
          : null;

    if (!body || body.type === "BlockStatement") return false;
    if (/[\r\n]/.test(doc.text.slice(current.start, body.start).trimEnd())) return false;

    current = body;
  }
}

function operation(doc: Doc, node: Node): string | null {
  if (node.type !== "ExpressionStatement") return null;

  const expression = node.expression;
  if (expression.type === "CallExpression") return `call:${source(doc, expression.callee)}`;
  if (expression.type === "AssignmentExpression") return `assign:${source(doc, expression.left)}`;

  return null;
}

function repeatsOperation(doc: Doc, root: Node, expected: string): boolean {
  const stack = [root];
  while (stack.length > 0) {
    const node = stack.pop()!;
    if (
      node.type === "FunctionExpression" ||
      node.type === "ArrowFunctionExpression" ||
      node.type === "FunctionDeclaration"
    )
      continue;

    if (node.type === "ExpressionStatement") {
      if (operation(doc, node) === expected) return true;
      continue;
    }

    for (const [, child] of children(node).toReversed()) stack.push(child);
  }

  return false;
}

export function facts(doc: Doc, node: Statement | SwitchCase): Item<Statement | SwitchCase> {
  const finalizer = node.type === "TryStatement" ? node.finalizer : null;

  return {
    node,
    start: node.start,
    end: node.end,
    kind: kindOf(node),
    word: WORDS[node.type] ?? null,
    binds: bindsOf(doc, node),
    declaration:
      node.type === "VariableDeclaration"
        ? { keyword: node.kind, letLike: node.kind === "let" }
        : null,
    guard: guardOf(node),
    compact: compact(doc, node),
    caseBody: node.type === "SwitchCase" && node.consequent.length > 0,
    operation: operation(doc, node),
    reads: (names) => firstReference(node, names),
    readsPath: (path) => readsPath(doc, node, path),
    finallyRepeats: (expected) => finalizer !== null && repeatsOperation(doc, finalizer, expected),
  };
}
