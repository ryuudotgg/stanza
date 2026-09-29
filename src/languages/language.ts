import type { Region } from "../engine/directives.ts";
import type { OffsetEdit } from "../engine/edits.ts";
import type { Comment, Doc, StatementList, Stmt } from "../engine/model.ts";
import type { Finding } from "../engine/types.ts";

export interface Rejection {
  start: number;
  message: string;
}

export interface Parsed {
  comments: Comment[];
  error: Rejection | undefined;
  rejection(text: string): Rejection | undefined;
  scan(doc: Doc): Scan;
}

export interface Scan {
  lists: StatementList[];
  frozen: Region[];
  braces: BraceScan | undefined;
}

export type Touches = (first: number, last: number) => boolean;

export interface BracePass {
  edits: OffsetEdit[];
  findings: Finding[];
}

export interface Opening {
  start: number;
  open: number;
  close: number;
  inner: [number, number] | null;
}

export type Hold =
  | { kind: "directive" }
  | { kind: "inapplicable"; reason: string }
  | { kind: "held"; reason: string | null };

export interface BraceScan {
  pass(touches: Touches, touched: ReadonlySet<Stmt>, collapseChains: boolean): BracePass;
  opening(line: number): Opening[];
  hold(start: number, at: (offset: number) => number): Hold;
}

export interface ConfigArm {
  setting(dir: string, extension: string): { enforced: boolean; unread: string[] };
  decisions(
    dir: string,
    extension: string,
  ): { setting: "on" | "off" | "unknown"; files: string[] }[];
}

export interface OracleStatement {
  start: number;
  clause: boolean;
  skeleton(): string;
  gaps(): [string | undefined, string | undefined];
}

export interface OracleSide {
  comments: Comment[];
  blocks: { start: number; end: number; single: boolean }[];
  shape(): string | undefined;
  container(comment: Comment): Region;
  statementAfter(offset: number): OracleStatement | undefined;
}

export interface Oracle {
  side(path: string, text: string): OracleSide;
}

export interface Language {
  parse(path: string, text: string): Parsed;
  config?: ConfigArm;
  oracle(): Oracle;
}

export interface Entry {
  id: string;
  extensions: readonly string[];
  directories: ReadonlySet<string>;
  skipsName(name: string, keepGenerated: boolean): boolean;
  load(): Language;
}
