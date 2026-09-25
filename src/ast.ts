import type { Node } from "oxc-parser";

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

export function children(node: Node): [string, Node][] {
  const result: [string, Node][] = [];
  for (const [key, value] of Object.entries(node)) {
    if (key === "parent") continue;
    const values: unknown[] = Array.isArray(value) ? value : [value];
    for (const child of values) if (isNode(child)) result.push([key, child]);
  }

  return result;
}

export function walk(
  node: Node,
  visit: (node: Node, parent: Node | null) => void,
  parent: Node | null = null,
): void {
  visit(node, parent);
  for (const [, child] of children(node)) walk(child, visit, node);
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

export function references(node: Node, names: Set<string>, binding = false): boolean {
  if (node.type === "Identifier") return !binding && names.has(node.name);
  if (node.type === "FunctionExpression" || node.type === "ArrowFunctionExpression") return false;

  return children(node).some(([key, child]) => {
    if (!referenceChild(node, key)) return false;

    const pattern =
      key === "params" ||
      (binding && key !== "right" && key !== "key") ||
      (node.type === "CatchClause" && key === "param");

    return references(child, names, pattern);
  });
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
