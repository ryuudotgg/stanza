import type { Comment, Program, Statement, SwitchCase } from "oxc-parser";
import type { GapRule } from "./rules.ts";

export interface Doc {
  path: string;
  text: string;
  lines: string[];
  lineStarts: number[];
  program: Program;
  comments: Comment[];
}

export interface Stmt {
  node: Statement | SwitchCase;
  frozen: boolean;
  startLine: number;
  codeStartLine: number;
  endLine: number;
  multiline: boolean;
  detached: boolean;
}

export type ListKind = "function" | "block" | "case" | "switch";

export interface StatementList {
  kind: ListKind;
  openLine: number | null;
  closeLine: number | null;
  stmts: Stmt[];
}

export type GapDecision =
  | { want: "none"; rule: GapRule<"none"> }
  | { want: "at-least-one"; rule: GapRule<"at-least-one"> }
  | { want: "keep" }
  | { want: "frozen" };

export interface Gap {
  prev: Stmt;
  next: Stmt;
  blank: number;
  decision: GapDecision;
}

export interface LineEdits {
  deleteLines: Set<number>;
  insertAfter: Set<number>;
}
