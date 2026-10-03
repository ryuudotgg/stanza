import { visitorKeys, type Node } from "oxc-parser";
import type { Path } from "../../engine/model.ts";

function isNode(value: unknown): value is Node {
  return (
    typeof value === "object" &&
    value !== null &&
    "type" in value &&
    typeof value.type === "string" &&
    "start" in value &&
    typeof value.start === "number" &&
    "end" in value &&
    typeof value.end === "number"
  );
}

function eachChild(node: Node, each: (key: string, child: Node) => void): void {
  const keys = visitorKeys[node.type];
  if (keys) {
    const fields = node as unknown as Record<string, unknown>;
    for (const key of keys) visitValue(key, fields[key], each);
  } else
    for (const [key, value] of Object.entries(node))
      if (key !== "parent") visitValue(key, value, each);
}

function visitValue(key: string, value: unknown, each: (key: string, child: Node) => void): void {
  if (Array.isArray(value)) {
    for (const child of value) if (isNode(child)) each(key, child);
  } else if (isNode(value)) each(key, value);
}

export function children(node: Node): [string, Node][] {
  const result: [string, Node][] = [];
  eachChild(node, (key, child) => result.push([key, child]));
  return result;
}

function pushValueReverse(
  value: unknown,
  parent: Node,
  nodes: Node[],
  parents: (Node | null)[],
): void {
  if (Array.isArray(value)) {
    for (let index = value.length - 1; index >= 0; index--) {
      const child: unknown = value[index];
      if (!isNode(child)) continue;

      nodes.push(child);
      parents.push(parent);
    }
  } else if (isNode(value)) {
    nodes.push(value);
    parents.push(parent);
  }
}

function pushChildrenReverse(node: Node, nodes: Node[], parents: (Node | null)[]): void {
  const keys = visitorKeys[node.type];
  if (keys) {
    const fields = node as unknown as Record<string, unknown>;
    for (let index = keys.length - 1; index >= 0; index--)
      pushValueReverse(fields[keys[index]!], node, nodes, parents);
  } else {
    const entries = Object.entries(node);
    for (let index = entries.length - 1; index >= 0; index--) {
      const [key, value] = entries[index]!;
      if (key === "parent") continue;
      pushValueReverse(value, node, nodes, parents);
    }
  }
}

export function walk(
  root: Node,
  enter: (node: Node, parent: Node | null) => void,
  leave?: (node: Node, parent: Node | null) => void,
): void {
  const nodes: Node[] = [root];
  const parents: (Node | null)[] = [null];
  if (!leave) {
    while (nodes.length > 0) {
      const node = nodes.pop()!;
      const parent = parents.pop()!;
      enter(node, parent);
      pushChildrenReverse(node, nodes, parents);
    }

    return;
  }

  const leaveIndexes: number[] = [];
  while (nodes.length > 0) {
    const node = nodes.pop()!;
    const parent = parents.pop()!;
    if (leaveIndexes.at(-1) === nodes.length) {
      leaveIndexes.pop();
      leave(node, parent);
      continue;
    }

    enter(node, parent);
    leaveIndexes.push(nodes.length);
    nodes.push(node);
    parents.push(parent);

    pushChildrenReverse(node, nodes, parents);
  }
}

export function boundNames(root: Node): Set<string> {
  const names = new Set<string>();
  const stack = [root];
  while (stack.length > 0) {
    const node = stack.pop()!;
    switch (node.type) {
      case "Identifier":
        names.add(node.name);
        break;

      case "VariableDeclaration":
        for (const declaration of node.declarations.toReversed()) stack.push(declaration.id);
        break;

      case "ObjectPattern":
        for (const property of node.properties.toReversed()) stack.push(property);
        break;

      case "ArrayPattern":
        for (const element of node.elements.toReversed()) if (element) stack.push(element);
        break;

      case "Property":
        stack.push(node.value);
        break;

      case "AssignmentPattern":
        stack.push(node.left);
        break;

      case "RestElement":
        stack.push(node.argument);
    }
  }

  return names;
}

export function referenceChild(node: Node, key: string): boolean {
  if (["id", "label", "typeAnnotation", "typeParameters", "returnType"].includes(key)) return false;
  if (node.type.startsWith("TS")) return key === "expression";
  if (key === "key" || (node.type === "MemberExpression" && key === "property"))
    return "computed" in node && node.computed === true;

  return true;
}

function lexicalNames(node: Node): string[] {
  if (node.type === "VariableDeclaration") return node.kind === "var" ? [] : [...boundNames(node)];
  if ((node.type === "FunctionDeclaration" || node.type === "ClassDeclaration") && node.id)
    return [node.id.name];

  return [];
}

export function declared(node: Node): string[] {
  switch (node.type) {
    case "ForStatement":
      return node.init ? lexicalNames(node.init) : [];

    case "ForInStatement":
    case "ForOfStatement":
      return lexicalNames(node.left);

    case "BlockStatement":
    case "StaticBlock":
      return node.body.flatMap(lexicalNames);

    case "SwitchStatement":
      return node.cases.flatMap((clause) => clause.consequent.flatMap(lexicalNames));

    case "CatchClause":
      return node.param ? [...boundNames(node.param)] : [];

    case "FunctionDeclaration":
      return node.params.flatMap((param) => [...boundNames(param)]);

    default:
      return [];
  }
}

export function jsxElementPath(node: Node): Path | undefined {
  if (node.type === "JSXIdentifier") return /^[a-z]|-/.test(node.name) ? undefined : [node.name];
  if (node.type !== "JSXMemberExpression") return undefined;

  const segments = [node.property.name];

  let head = node.object;
  while (head.type === "JSXMemberExpression") {
    segments.push(head.property.name);
    head = head.object;
  }

  return [head.name, ...segments.reverse()];
}

export function* references(root: Node, names: Set<string>): Generator<string> {
  const stack = [{ node: root, names, binding: false }];
  while (stack.length > 0) {
    const { node, names, binding } = stack.pop()!;
    if (node.type === "Identifier") {
      if (!binding && names.has(node.name)) yield node.name;
      continue;
    }

    if (node.type === "FunctionExpression" || node.type === "ArrowFunctionExpression") continue;
    if (node.type === "FunctionDeclaration" && node !== root) continue;

    const shadowed = declared(node);
    const visible = shadowed.some((name) => names.has(name))
      ? names.difference(new Set(shadowed))
      : names;

    if (visible.size === 0) continue;

    if (node.type === "JSXOpeningElement") {
      const path = jsxElementPath(node.name);
      if (!binding && path && visible.has(path[0])) yield path[0];
    }

    const next = children(node);
    for (let index = next.length - 1; index >= 0; index--) {
      const [key, child] = next[index]!;
      if (!referenceChild(node, key)) continue;

      const pattern =
        key === "params" ||
        (binding && key !== "right" && key !== "key") ||
        (node.type === "CatchClause" && key === "param");

      stack.push({ node: child, names: visible, binding: pattern });
    }
  }
}

export const LOOP_TYPES = new Set([
  "ForStatement",
  "ForInStatement",
  "ForOfStatement",
  "WhileStatement",
  "DoWhileStatement",
]);

export const JUMP_TYPES = new Set([
  "ReturnStatement",
  "ThrowStatement",
  "ContinueStatement",
  "BreakStatement",
]);
