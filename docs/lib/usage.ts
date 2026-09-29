import type { Code, Root, Table, TableCell } from "mdast";
import { flags, usage } from "../../src/usage";
import { phrasing } from "./rules";

function cell(children: TableCell["children"]): TableCell {
  return { type: "tableCell", children };
}

function flagTable(): Table {
  return {
    type: "table",
    children: [
      {
        type: "tableRow",
        children: [
          cell([{ type: "text", value: "flag" }]),
          cell([{ type: "text", value: "what it does" }]),
        ],
      },
      ...flags.map(([flag, description]) => ({
        type: "tableRow" as const,
        children: [cell([{ type: "inlineCode", value: flag }]), cell(phrasing(description))],
      })),
    ],
  };
}

function usageBlock(): Code {
  return { type: "code", lang: "text", value: usage };
}

export function remarkUsage() {
  return (tree: Root) => {
    for (const [index, node] of tree.children.entries()) {
      if (node.type !== "mdxJsxFlowElement") continue;

      if (node.name === "FlagTable") tree.children[index] = flagTable();
      if (node.name === "Usage") tree.children[index] = usageBlock();
    }
  };
}
