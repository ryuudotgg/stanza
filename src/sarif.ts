import { version } from "../package.json" with { type: "json" };
import { sep } from "node:path";
import { RULES } from "./rules.ts";
import type { Finding } from "./types.ts";

export function sarifLog(findings: Finding[]): string {
  const entries = Object.entries(RULES);
  const indices = new Map(entries.map(([id], index) => [id, index]));

  return JSON.stringify({
    $schema: "https://json.schemastore.org/sarif-2.1.0.json",
    version: "2.1.0",
    runs: [
      {
        tool: {
          driver: {
            name: "stanza",
            version,
            informationUri: "https://github.com/ryuudotgg/stanza",
            rules: entries.map(([id, rule]) => ({ id, shortDescription: { text: rule.summary } })),
          },
        },
        results: findings.map((finding) => {
          const ruleIndex = indices.get(finding.rule);
          const uri = finding.path.split(sep).map(encodeURIComponent).join("/");
          return {
            ruleId: finding.rule,
            ...(ruleIndex === undefined ? {} : { ruleIndex }),
            level: ruleIndex === undefined ? "error" : "warning",
            message: { text: finding.message },
            locations: [
              {
                physicalLocation: {
                  artifactLocation: { uri },
                  region: { startLine: finding.line, startColumn: finding.col },
                },
              },
            ],
          };
        }),
      },
    ],
  });
}
