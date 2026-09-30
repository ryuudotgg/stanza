import type { Code, Root, Table, TableCell } from "mdast";
import { flags, usage } from "../../src/usage";
import { phrasing } from "./rules";

const flagLinks: Record<string, string | null> = {
  "--fix": "/reference/output#findings",
  "--check": "/reference/output#findings",
  "--changed": "/reference/file-selection#changed-files",
  "--hunks": "/reference/file-selection#hunks",
  "--staged": "/reference/file-selection#staged",
  "--stdin <path>": "/reference/output#stdin",
  "--json": "/reference/output#json",
  "--braces": "/rules/braces-and-lint-config",
  "--no-braces": "/rules/braces-and-lint-config",
  "explain <file>:<line>": "/reference/explain",
  hook: "/reference/hook-protocol",
  "--help": null,
  "--version": null,
};

function cell(children: TableCell["children"]): TableCell {
  return { type: "tableCell", children };
}

function flagTable(): Table {
  const stale = Object.keys(flagLinks).filter((flag) => !flags.some(([name]) => name === flag));
  if (stale.length > 0)
    throw new Error(`FlagTable: link entries for unknown flags ${stale.join(", ")}`);

  const rows = flags.map(([flag, description]) => {
    if (!Object.hasOwn(flagLinks, flag)) throw new Error(`FlagTable: no link entry for ${flag}`);

    const code = { type: "inlineCode" as const, value: flag };
    const href = flagLinks[flag];
    const label = href ? { type: "link" as const, url: href, children: [code] } : code;
    return {
      type: "tableRow" as const,
      children: [cell([label]), cell(phrasing(description))],
    };
  });

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
      ...rows,
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
