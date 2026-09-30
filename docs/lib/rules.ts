import { basename, extname } from "node:path";
import type { Paragraph, PhrasingContent, Root, Table, TableRow } from "mdast";
import type {} from "mdast-util-mdx-jsx";
import { visit } from "unist-util-visit";
import { RULES, type RuleId } from "../../src/engine/rules";

function isRuleId(id: string): id is RuleId {
  return Object.hasOwn(RULES, id);
}

export function ruleSentence(id: RuleId): string {
  const rule = RULES[id];
  return `${rule.fixable ? rule.summary[0]!.toUpperCase() + rule.summary.slice(1) : `Reports ${rule.summary}`}.`;
}

export function phrasing(summary: string): PhrasingContent[] {
  return summary
    .split("`")
    .flatMap((value, index): PhrasingContent[] =>
      value === ""
        ? []
        : [index % 2 === 1 ? { type: "inlineCode", value } : { type: "text", value }],
    );
}

function ruleTable(kind: "fix" | "check"): Table {
  const fixable = kind === "fix";
  const header: TableRow = {
    type: "tableRow",
    children: [
      { type: "tableCell", children: [{ type: "text", value: "rule" }] },
      {
        type: "tableCell",
        children: [{ type: "text", value: fixable ? "what it does" : "what it reports" }],
      },
    ],
  };

  const rows: TableRow[] = Object.entries(RULES)
    .filter(([, rule]) => rule.fixable === fixable)
    .map(([id, rule]) => ({
      type: "tableRow",
      children: [
        {
          type: "tableCell",
          children: [
            { type: "link", url: `/rules/${id}`, children: [{ type: "inlineCode", value: id }] },
          ],
        },
        { type: "tableCell", children: phrasing(rule.summary) },
      ],
    }));

  return { type: "table", children: [header, ...rows] };
}

function dropIncludedFinalNewline(tree: Root) {
  visit(tree, "code", (node) => {
    if (node.value.endsWith("\n")) node.value = node.value.slice(0, -1);
  });
}

export function remarkRules() {
  return (tree: Root, file: { path: string }) => {
    dropIncludedFinalNewline(tree);

    for (const [index, node] of tree.children.entries()) {
      if (node.type !== "mdxJsxFlowElement") continue;

      if (node.name === "RuleSummary") {
        const id = basename(file.path, extname(file.path));
        if (!isRuleId(id)) throw new Error(`RuleSummary: unknown rule id "${id}" in ${file.path}`);

        const paragraph: Paragraph = { type: "paragraph", children: phrasing(ruleSentence(id)) };
        tree.children[index] = paragraph;
      }

      if (node.name === "RuleTable") {
        const kind = node.attributes.find(
          (attribute) => attribute.type === "mdxJsxAttribute" && attribute.name === "kind",
        )?.value;

        if (kind !== "fix" && kind !== "check")
          throw new Error(`RuleTable: expected kind="fix" or kind="check" in ${file.path}`);

        tree.children[index] = ruleTable(kind);
      }
    }
  };
}
