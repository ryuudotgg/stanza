import type { GapRule } from "./rules.ts";

export interface Comment {
  type: "Line" | "Block";
  value: string;
  start: number;
  end: number;
}

export interface Doc {
  path: string;
  text: string;
  lines: string[];
  lineStarts: number[];
  comments: Comment[];
}

export type Kind =
  | "declaration"
  | "assignment"
  | "if"
  | "loop"
  | "switch"
  | "case"
  | "try"
  | "function"
  | "return"
  | "throw"
  | "break"
  | "continue"
  | "expression"
  | "other";

export type Path = [string, ...string[]];

export type Binding = Set<string> | Path;

export interface Facts {
  kind: Kind;
  word: string | null;
  binds: Binding | null;
  declaration: { keyword: string; letLike: boolean } | null;
  guard: { jump: string | null } | null;
  compact: boolean;
  joined: number | null;
  caseBody: boolean;
  operation: string | null;
  references: (names: Set<string>) => Iterable<string>;
  readsPath: (path: Path) => boolean;
  finallyRepeats: (operation: string) => boolean;
}

export interface Item<Node = unknown> extends Facts {
  node: Node;
  start: number;
  end: number;
}

export interface Stmt<Node = unknown> extends Item<Node> {
  frozen: boolean;
  startLine: number;
  endLine: number;
  multiline: boolean;
  detached: boolean;
}

export type ListKind = "block" | "switch";

export interface StatementList<Node = unknown> {
  kind: ListKind;
  start: number;
  end: number;
  openLine: number | null;
  closeLine: number | null;
  stmts: Stmt<Node>[];
}

export type JoinRule = "guard-join" | "consume-join" | "use-join";

export type GapDecision =
  | { want: "none"; rule: JoinRule; name: string; reader: string }
  | { want: "none"; rule: Exclude<GapRule<"none">, JoinRule> }
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

export interface Changed {
  lines: ReadonlySet<number>;
  deletedAfter: ReadonlySet<number>;
}
