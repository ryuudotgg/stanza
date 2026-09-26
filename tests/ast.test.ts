import { expect, test } from "bun:test";
import { Glob } from "bun";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { parseSync, type Node } from "oxc-parser";
import { children, walk } from "../src/ast.ts";

test("walk falls back to object properties for unknown node types", () => {
  const block = parseSync("fixture.ts", "{ first(); second(); }").program.body[0]!;
  const root = {
    type: "StanzaUnknownNode",
    start: 0,
    end: block.end,
    child: block,
  } as unknown as Node;

  const visited: { type: string; parent: string | null }[] = [];
  walk(root, (node, parent) => visited.push({ type: node.type, parent: parent?.type ?? null }));

  expect(
    visited.filter(({ parent }) => parent === "StanzaUnknownNode").map(({ type }) => type),
  ).toEqual(["BlockStatement"]);

  expect(
    visited.filter(({ parent }) => parent === "BlockStatement").map(({ type }) => type),
  ).toEqual(["ExpressionStatement", "ExpressionStatement"]);
});

test("walk visits nodes in recursive enter and leave order", () => {
  const program = parseSync("fixture.ts", "if (a) { b(); }").program;
  const visited: string[] = [];

  walk(
    program,
    (node) => visited.push(`enter:${node.type}`),
    (node) => visited.push(`leave:${node.type}`),
  );

  expect(visited).toEqual([
    "enter:Program",
    "enter:IfStatement",
    "enter:Identifier",
    "leave:Identifier",
    "enter:BlockStatement",
    "enter:ExpressionStatement",
    "enter:CallExpression",
    "enter:Identifier",
    "leave:Identifier",
    "leave:CallExpression",
    "leave:ExpressionStatement",
    "leave:BlockStatement",
    "leave:IfStatement",
    "leave:Program",
  ]);
});

function enumerated(node: Node): [string, Node][] {
  const result: [string, Node][] = [];
  for (const [key, value] of Object.entries(node)) {
    if (key === "parent") continue;
    // oxc visitorKeys leave out Program.hashbang, a leaf stanza never formats.
    if (node.type === "Program" && key === "hashbang") continue;
    for (const child of Array.isArray(value) ? value : [value])
      if (typeof child?.type === "string" && typeof child.start === "number")
        result.push([key, child]);
  }

  return result;
}

test("children matches every node property for the types in the fixtures and source", () => {
  const root = join(import.meta.dir, "..");
  const files = ["src/**/*.ts", "tests/fixtures/**/*.ts"].flatMap((pattern) => [
    ...new Glob(pattern).scanSync(root),
  ]);

  const mismatched = new Set<string>();

  for (const file of files) {
    const program = parseSync(file, readFileSync(join(root, file), "utf8")).program;
    walk(program, (node) => {
      if (!Bun.deepEquals(children(node), enumerated(node))) mismatched.add(node.type);
    });
  }

  expect(files.length).toBeGreaterThan(20);
  expect([...mismatched]).toEqual([]);
});
