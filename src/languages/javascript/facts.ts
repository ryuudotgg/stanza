import type { Node, Statement, SwitchCase } from "oxc-parser";
import { boundNames, children, declared, jsxElementPath, references } from "./ast.ts";
import { lineAt, source } from "../../engine/doc.ts";
import type { Binding, Doc, Item, Kind, Path } from "../../engine/model.ts";
import type { Layout } from "../language.ts";

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

function exported(node: Statement | SwitchCase): Statement | SwitchCase {
  if (node.type === "ExportNamedDeclaration" && node.declaration) return node.declaration;
  if (node.type === "ExportDefaultDeclaration" && node.declaration.type === "FunctionDeclaration")
    return node.declaration;

  return node;
}

function kindOf(node: Statement | SwitchCase): Kind {
  if (
    node.type === "ImportDeclaration" ||
    node.type === "ExportAllDeclaration" ||
    (node.type === "ExportNamedDeclaration" && node.source)
  )
    return "import";

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

    if (node.type === "JSXOpeningElement") {
      const elementPath = jsxElementPath(node.name);
      if (
        elementPath &&
        elementPath.length >= path.length &&
        path.every((segment, index) => elementPath[index] === segment)
      )
        return true;
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

function joinedArm(doc: Doc, start: number, body: Node): string | null {
  if (body.type === "BlockStatement") return null;

  const header = doc.text.slice(start, body.start);
  const lines = header.split(/\r?\n/);
  if (lines.length > 2 || (lines.length === 2 && lines[1]!.trim() !== "")) return null;

  let before = body.start;
  while (before > start && /\s/.test(doc.text[before - 1]!)) before--;

  if (doc.comments.some((comment) => comment.end === before && comment.start >= start)) return null;

  const joined = joinedText(doc, body);
  return joined === null
    ? null
    : `${lines[0]!.trimEnd()}${body.type === "EmptyStatement" ? "" : " "}${joined}`;
}

function joinedText(doc: Doc, node: Node): string | null {
  if (lineAt(doc, node.start) === lineAt(doc, node.end - 1)) return source(doc, node).trim();

  const body =
    node.type === "IfStatement" && !node.alternate
      ? node.consequent
      : node.type === "ForStatement" ||
          node.type === "ForInStatement" ||
          node.type === "ForOfStatement" ||
          node.type === "WhileStatement"
        ? node.body
        : null;

  return body && joinedArm(doc, node.start, body);
}

function joinedArms(doc: Doc, node: Node): string[] | null {
  if (node.type !== "IfStatement") {
    const joined = joinedText(doc, node);
    return joined === null ? null : [joined];
  }

  const { consequent, alternate } = node;
  const first = joinedArm(doc, node.start, consequent);
  if (first === null) return null;
  if (!alternate) return [first];

  if (
    doc.comments.some(
      (comment) => comment.start >= consequent.end && comment.end <= alternate.start,
    )
  )
    return null;

  if (alternate.type === "IfStatement") {
    const rest = joinedArms(doc, alternate);
    return rest && [first, `else ${rest[0]}`, ...rest.slice(1)];
  }

  const last = joinedArm(doc, doc.text.indexOf("else", consequent.end), alternate);
  return last === null ? null : [first, last];
}

function joinedColumns(doc: Doc, node: Node, width: Layout): number | null {
  if (lineAt(doc, node.start) === lineAt(doc, node.end - 1)) return null;

  const arms = joinedArms(doc, node);
  if (arms === null) return null;

  const lineStart = doc.lineStarts[lineAt(doc, node.start) - 1]!;
  const prefix = doc.text.slice(lineStart, node.start);
  const indent = /^\s*/.exec(prefix)![0];

  let widest = 0;
  for (const [index, arm] of arms.entries()) {
    let columns = 0;
    for (const char of (index === 0 ? prefix : indent) + arm)
      columns += char === "\t" ? width.tab - (columns % width.tab) : Bun.stringWidth(char);

    widest = Math.max(widest, columns);
  }

  return widest;
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

export function facts(
  doc: Doc,
  node: Statement | SwitchCase,
  width: Layout,
): Item<Statement | SwitchCase> {
  const inner = exported(node);
  const finalizer = node.type === "TryStatement" ? node.finalizer : null;
  const joined = joinedColumns(doc, node, width);
  return {
    node,
    start: node.start,
    end: node.end,
    kind: kindOf(inner),
    word: WORDS[inner.type] ?? null,
    binds: bindsOf(doc, inner),
    declaration:
      inner.type === "VariableDeclaration"
        ? { keyword: inner.kind, letLike: inner.kind === "let" }
        : null,
    guard: guardOf(node),
    compact:
      lineAt(doc, node.start) === lineAt(doc, node.end - 1) ||
      (joined !== null && joined <= width.columns),
    joined,
    caseBody: node.type === "SwitchCase" && node.consequent.length > 0,
    operation: operation(doc, node),
    references: (names) => references(inner, names),
    readsPath: (path) => readsPath(doc, inner, path),
    finallyRepeats: (expected) => finalizer !== null && repeatsOperation(doc, finalizer, expected),
  };
}
