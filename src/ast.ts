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

export function walk(
  node: Node,
  enter: (node: Node, parent: Node | null) => void,
  leave?: (node: Node, parent: Node | null) => void,
  parent: Node | null = null,
): void {
  enter(node, parent);
  eachChild(node, (_key, child) => walk(child, enter, leave, node));
  leave?.(node, parent);
}

export function boundNames(node: Node): Set<string> {
  switch (node.type) {
    case "Identifier":
      return new Set([node.name]);

    case "VariableDeclaration":
      return new Set(node.declarations.flatMap((declaration) => [...boundNames(declaration.id)]));

    case "ObjectPattern":
      return new Set(node.properties.flatMap((property) => [...boundNames(property)]));

    case "ArrayPattern":
      return new Set(node.elements.flatMap((element) => (element ? [...boundNames(element)] : [])));

    case "Property":
      return boundNames(node.value);

    case "AssignmentPattern":
      return boundNames(node.left);

    case "RestElement":
      return boundNames(node.argument);

    default:
      return new Set();
  }
}

function referenceChild(node: Node, key: string): boolean {
  if (["id", "label", "typeAnnotation", "typeParameters", "returnType"].includes(key)) return false;
  if (node.type.startsWith("TS")) return key === "expression";
  if (key === "key" || (node.type === "MemberExpression" && key === "property"))
    return "computed" in node && node.computed === true;

  return true;
}

export function firstReference(
  node: Node,
  names: Set<string>,
  binding = false,
): string | undefined {
  if (node.type === "Identifier") return !binding && names.has(node.name) ? node.name : undefined;
  if (node.type === "FunctionExpression" || node.type === "ArrowFunctionExpression")
    return undefined;

  for (const [key, child] of children(node)) {
    if (!referenceChild(node, key)) continue;

    const pattern =
      key === "params" ||
      (binding && key !== "right" && key !== "key") ||
      (node.type === "CatchClause" && key === "param");

    const name = firstReference(child, names, pattern);
    if (name !== undefined) return name;
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
