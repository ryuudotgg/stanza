import type { Root } from "mdast";
import { visit } from "unist-util-visit";

export function remarkCodeNewline() {
  return (tree: Root) => {
    visit(tree, "code", (node) => {
      if (node.value.endsWith("\n")) node.value = node.value.slice(0, -1);
    });
  };
}
