export type RuleId =
  | "after-multiline"
  | "switch-clauses"
  | "edge-blank"
  | "guard-join"
  | "consume-join"
  | "use-join"
  | "guard-chain"
  | "let-step"
  | "after-guard"
  | "short-body"
  | "braces"
  | "block-spacing"
  | "wall";

export const FIXABLE_RULES: ReadonlySet<RuleId> = new Set<RuleId>([
  "after-multiline",
  "switch-clauses",
  "edge-blank",
  "guard-join",
  "consume-join",
  "use-join",
  "guard-chain",
  "let-step",
  "after-guard",
  "short-body",
  "braces",
]);

export type Mode = "fix" | "check";

export interface Options {
  enforcedBraces: boolean;
}

export interface Finding {
  path: string;
  line: number;
  col: number;
  rule: RuleId | "parse";
  message: string;
  fixable: boolean;
}

export interface FileResult {
  text: string;
  findings: Finding[];
  parseError: boolean;
}
