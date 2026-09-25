import { expect, test } from "bun:test";
import { parseSync, type Node } from "oxc-parser";
import { walk } from "../src/ast.ts";

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
