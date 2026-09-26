import type { RuleId } from "./rules.ts";

export type Mode = "fix" | "check";

export interface Options {
  keepBraces: boolean;
}

export interface Finding {
  path: string;
  line: number;
  col: number;
  rule: RuleId | "parse" | "write";
  message: string;
  fixable: boolean;
}

export interface FileResult {
  text: string;
  findings: Finding[];
  parseError: boolean;
}
