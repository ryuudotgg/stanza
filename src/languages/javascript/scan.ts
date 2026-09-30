import type { Node, Program } from "oxc-parser";
import { freeze, ignored, marks, within, type Region } from "../../engine/directives.ts";
import type { Doc } from "../../engine/model.ts";
import type { Layout, Scan } from "../language.ts";
import { braceScan, controlledBlocks } from "./braces.ts";
import { listAt, type List } from "./lists.ts";

const CONTAINERS = new Set([
  "BlockStatement",
  "StaticBlock",
  "SwitchStatement",
  "SwitchCase",
  "ClassBody",
  "TSModuleBlock",
]);

export function scan(doc: Doc, program: Program, width: Layout): Scan {
  const marked = marks(doc);
  const containers: Region[] = [];
  const escaped = new Set<Node>();

  const lists: List[] = [];
  const chains = new Map<Node, Node>();
  const frozenOwners = new Set<Node>();

  const owners = controlledBlocks(program, (node, parent) => {
    const list = listAt(doc, node, width);
    if (list) lists.push(list);

    if (marked.length > 0)
      if (
        parent !== null &&
        (escaped.has(parent) || node.start < parent.start || parent.end < node.end)
      )
        escaped.add(node);
      else if (CONTAINERS.has(node.type)) containers.push({ start: node.start, end: node.end });

    if (node.type === "IfStatement")
      chains.set(
        node,
        parent?.type === "IfStatement" && parent.alternate === node
          ? (chains.get(parent) ?? parent)
          : node,
      );

    if (
      (node.type.endsWith("Statement") && ignored(doc, node.start)) ||
      (parent !== null &&
        frozenOwners.has(parent) &&
        ((parent.type === "IfStatement" && parent.alternate === node) ||
          parent.type === "LabeledStatement"))
    )
      frozenOwners.add(node);
  });

  const { lists: active, frozen } = freeze(doc, marked, lists, containers);
  const blocks = [...owners]
    .filter(
      ([block, owner]) =>
        !frozenOwners.has(owner) &&
        !frozenOwners.has(block) &&
        !within(frozen, block.start) &&
        !within(frozen, block.end - 1),
    )
    .map(([block]) => block);

  return { lists: active, frozen, braces: braceScan(doc, blocks, owners, chains) };
}
