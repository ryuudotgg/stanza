import { blankLines, commentIndex, lineAt } from "./doc.ts";
import { ignored } from "./directives.ts";
import type { Doc, Item, Stmt } from "./model.ts";

function fallthroughEnd<Node>(
  doc: Doc,
  item: Item<Node>,
  next: Item<Node> | undefined,
  end: number,
): number {
  if (!item.caseBody || next?.kind !== "case") return end;

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

export function attach<Node>(
  doc: Doc,
  items: Item<Node>[],
  opener: number,
  limit: number,
): Stmt<Node>[] {
  let previousEnd = opener;
  return items.map((item, itemIndex) => {
    const itemStartLine = lineAt(doc, item.start);
    const itemEndLine = lineAt(doc, item.end - 1);
    const leading = doc.comments[commentIndex(doc, previousEnd)];
    const leadLine =
      leading && leading.end <= item.start ? lineAt(doc, leading.start) : itemStartLine;

    const detached =
      leadLine !== itemStartLine && blankLines(doc, leadLine - 1, itemStartLine).length > 0;

    let end = item.end;
    for (let index = commentIndex(doc, item.end); index < doc.comments.length; index++) {
      const comment = doc.comments[index]!;
      if (comment.start >= limit || lineAt(doc, comment.start) !== itemEndLine) break;
      end = Math.max(end, comment.end);
    }

    if (item.kind === "case") end = fallthroughEnd(doc, item, items[itemIndex + 1], end);

    previousEnd = end;
    return {
      ...item,
      frozen: ignored(doc, item.start),
      startLine: detached ? itemStartLine : leadLine,
      endLine: lineAt(doc, end - 1),
      multiline: itemStartLine !== itemEndLine,
      detached,
    };
  });
}
