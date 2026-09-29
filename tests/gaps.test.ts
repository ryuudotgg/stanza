import { expect, test } from "bun:test";
import { decide } from "../src/gaps.ts";
import type { StatementList, Stmt } from "../src/model.ts";

function stmt(line: number, facts: Partial<Stmt>): Stmt {
  return {
    node: null,
    start: line * 10,
    end: line * 10 + 9,
    kind: "other",
    word: null,
    binds: null,
    declaration: null,
    guard: null,
    compact: true,
    caseBody: false,
    operation: null,
    reads: () => undefined,
    readsPath: () => false,
    finallyRepeats: () => false,
    frozen: false,
    startLine: line,
    endLine: line,
    multiline: false,
    detached: false,
    ...facts,
  };
}

function list(next: Stmt): StatementList {
  return {
    kind: "block",
    start: 0,
    end: 100,
    openLine: 1,
    closeLine: 9,
    stmts: [declaration, next, stmt(5, { kind: "expression" }), stmt(6, { kind: "expression" })],
  };
}

const declaration = stmt(2, {
  kind: "declaration",
  declaration: { keyword: "const", letLike: false },
  binds: new Set(["x"]),
});

function reader(word: string): Stmt {
  return stmt(4, {
    kind: "if",
    word,
    reads: (names) => (names.has("x") ? "x" : undefined),
  });
}

test("the ladder joins an if to the declaration it reads, from facts alone", () => {
  const next = reader("if");
  expect(decide(list(next), declaration, next)).toEqual({
    want: "none",
    rule: "guard-join",
    name: "x",
    reader: "if",
  });
});

test("a guard word changes only the reader the join names", () => {
  const next = reader("guard");
  expect(decide(list(next), declaration, next)).toEqual({
    want: "none",
    rule: "guard-join",
    name: "x",
    reader: "guard",
  });
});
