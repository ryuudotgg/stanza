import { visitorKeys, type Node } from "oxc-parser";

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

function reverseFrom(stack: unknown[], first: number): void {
  for (let left = first, right = stack.length - 1; left < right; left++, right--) {
    const swapped = stack[left];
    stack[left] = stack[right];
    stack[right] = swapped;
  }
}

export function walk(
  root: Node,
  enter: (node: Node, parent: Node | null) => void,
  leave?: (node: Node, parent: Node | null) => void,
): void {
  const stack = [{ node: root, parent: null as Node | null, leaving: false }];
  while (stack.length > 0) {
    const frame = stack.pop()!;
    const { node, parent } = frame;
    if (frame.leaving) {
      leave?.(node, parent);
      continue;
    }

    enter(node, parent);
    frame.leaving = true;
    stack.push(frame);

    const first = stack.length;
    eachChild(node, (_key, child) => stack.push({ node: child, parent: node, leaving: false }));
    reverseFrom(stack, first);
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

function referenceChild(node: Node, key: string): boolean {
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

function declared(node: Node): string[] {
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

export function firstReference(root: Node, names: Set<string>): string | undefined {
  const stack = [{ node: root, names, binding: false }];
  while (stack.length > 0) {
    const { node, names, binding } = stack.pop()!;
    if (node.type === "Identifier") {
      if (!binding && names.has(node.name)) return node.name;
      continue;
    }

    if (node.type === "FunctionExpression" || node.type === "ArrowFunctionExpression") continue;
    if (node.type === "FunctionDeclaration" && node !== root) continue;

    const shadowed = declared(node);
    const visible = shadowed.length === 0 ? names : names.difference(new Set(shadowed));
    if (visible.size === 0) continue;

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

  return undefined;
}

export const BLOCK_TYPES = new Set([
  "IfStatement",
  "ForStatement",
  "ForInStatement",
  "ForOfStatement",
  "WhileStatement",
  "DoWhileStatement",
  "TryStatement",
  "SwitchStatement",
]);
