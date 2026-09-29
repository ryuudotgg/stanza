import type { Node, Statement, SwitchCase } from "oxc-parser";
import { attach } from "./attach.ts";
import { lineAt, nextToken } from "./doc.ts";
import { facts } from "./facts.ts";
import type { Doc, StatementList } from "./model.ts";

export type List = StatementList<Statement | SwitchCase>;

export function listAt(doc: Doc, node: Node): List | undefined {
  if (node.type === "BlockStatement" || node.type === "StaticBlock") {
    const start =
      node.type === "StaticBlock" ? nextToken(doc, node.start + "static".length) : node.start;

    return {
      kind: "block",
      start,
      end: node.end,
      openLine: lineAt(doc, start),
      closeLine: lineAt(doc, node.end - 1),
      stmts: attach(
        doc,
        node.body.map((stmt) => facts(doc, stmt)),
        start + 1,
        node.end - 1,
      ),
    };
  }

  if (node.type === "SwitchStatement" || node.type === "SwitchCase") {
    const nodes = node.type === "SwitchStatement" ? node.cases : node.consequent;
    const opener =
      node.type === "SwitchCase" ? (node.test?.end ?? node.start) : node.discriminant.end;

    return {
      kind: node.type === "SwitchStatement" ? "switch" : "block",
      start: node.start,
      end: node.end,
      openLine: null,
      closeLine: null,
      stmts: attach(
        doc,
        nodes.map((stmt) => facts(doc, stmt)),
        opener,
        node.end,
      ),
    };
  }
}
