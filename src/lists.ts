import type { Node, Statement, SwitchCase } from "oxc-parser";
import { blankLines, commentIndex, lineAt, nextToken } from "./doc.ts";
import { ignored } from "./directives.ts";
import type { Doc, StatementList, Stmt } from "./model.ts";

export type List = StatementList & { start: number; end: number };

function fallthroughEnd(doc: Doc, node: SwitchCase, next: Node | undefined, end: number): number {
  if (node.consequent.length === 0 || next?.type !== "SwitchCase") return end;

  const index = commentIndex(doc, end);
  const comment = doc.comments[index];
  if (!comment || (doc.comments[index + 1]?.start ?? Infinity) < next.start) return end;

  const directlyBetween =
    lineAt(doc, comment.start) === lineAt(doc, end - 1) + 1 &&
    lineAt(doc, comment.end - 1) + 1 === lineAt(doc, next.start);

  return directlyBetween && /^(falls through|fallthrough)/i.test(comment.value.trimStart())
    ? comment.end
    : end;
}

function statements(
  doc: Doc,
  nodes: (Statement | SwitchCase)[],
  opener: number,
  limit: number,
): Stmt[] {
  let previousEnd = opener;
  return nodes.map((node, nodeIndex) => {
    const codeStartLine = lineAt(doc, node.start);
    const nodeEndLine = lineAt(doc, node.end - 1);
    const leading = doc.comments[commentIndex(doc, previousEnd)];
    const leadLine =
      leading && leading.end <= node.start ? lineAt(doc, leading.start) : codeStartLine;

    const detached =
      leadLine !== codeStartLine && blankLines(doc, leadLine - 1, codeStartLine).length > 0;

    let end = node.end;
    for (let index = commentIndex(doc, node.end); index < doc.comments.length; index++) {
      const comment = doc.comments[index]!;
      if (comment.start >= limit || lineAt(doc, comment.start) !== nodeEndLine) break;
      end = Math.max(end, comment.end);
    }

    if (node.type === "SwitchCase") end = fallthroughEnd(doc, node, nodes[nodeIndex + 1], end);

    previousEnd = end;
    return {
      node,
      frozen: ignored(doc, node.start),
      startLine: detached ? codeStartLine : leadLine,
      codeStartLine,
      endLine: lineAt(doc, end - 1),
      multiline: codeStartLine !== nodeEndLine,
      detached,
    };
  });
}

function functionParent(parent: Node | null): boolean {
  return (
    parent?.type === "FunctionDeclaration" ||
    parent?.type === "FunctionExpression" ||
    parent?.type === "ArrowFunctionExpression"
  );
}

export function listAt(doc: Doc, node: Node, parent: Node | null): List | undefined {
  if (node.type === "BlockStatement" || node.type === "StaticBlock") {
    const start =
      node.type === "StaticBlock" ? nextToken(doc, node.start + "static".length) : node.start;

    return {
      kind: functionParent(parent) ? "function" : "block",
      start,
      end: node.end,
      openLine: lineAt(doc, start),
      closeLine: lineAt(doc, node.end - 1),
      stmts: statements(doc, node.body, start + 1, node.end - 1),
    };
  }

  if (node.type === "SwitchStatement" || node.type === "SwitchCase") {
    const nodes = node.type === "SwitchStatement" ? node.cases : node.consequent;
    const opener =
      node.type === "SwitchCase" ? (node.test?.end ?? node.start) : node.discriminant.end;

    return {
      kind: node.type === "SwitchStatement" ? "switch" : "case",
      start: node.start,
      end: node.end,
      openLine: null,
      closeLine: null,
      stmts: statements(doc, nodes, opener, node.end),
    };
  }
}
