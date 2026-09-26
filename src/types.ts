import type { RuleId } from "./rules.ts";

export type Mode = "fix" | "check";

export interface Options {
  keepBraces: boolean;
  changedLines?: ReadonlySet<number>;
}

export interface Finding {
  path: string;
  line: number;
  col: number;
  rule: RuleId | "parse" | "error" | "write";
  message: string;
  fixable: boolean;
}

export function compareFindings(left: Finding, right: Finding): number {
  return (
    left.path.localeCompare(right.path) ||
    left.line - right.line ||
    left.col - right.col ||
    left.rule.localeCompare(right.rule)
  );
}

export interface FileResult {
  text: string;
  findings: Finding[];
  parseError: boolean;
}
