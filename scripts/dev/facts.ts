import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { document } from "../../src/engine/doc.ts";
import type { Binding, Doc, Stmt } from "../../src/engine/model.ts";
import { languageOf } from "../../src/languages/index.ts";
import { FALLBACK_WIDTH, type Layout } from "../../src/languages/language.ts";
import { decode, withoutMark } from "../../src/step.ts";

function binding(binds: Binding | null): unknown {
  if (binds === null) return null;
  return binds instanceof Set ? [...binds] : { path: binds };
}

function questions(prev: Stmt, next: Stmt): Record<string, unknown> {
  return {
    references: prev.binds instanceof Set ? [...next.references(prev.binds)] : null,
    readsPath:
      prev.binds !== null && !(prev.binds instanceof Set) ? next.readsPath(prev.binds) : null,
    finallyRepeats: prev.operation === null ? null : next.finallyRepeats(prev.operation),
  };
}

function line(file: string, list: number, index: number, stmts: Stmt[]): string {
  const stmt = stmts[index]!;
  return JSON.stringify({
    file,
    list,
    stmt: index,
    start: stmt.start,
    end: stmt.end,
    kind: stmt.kind,
    word: stmt.word,
    binds: binding(stmt.binds),
    declaration: stmt.declaration,
    guard: stmt.guard,
    compact: stmt.compact,
    joined: stmt.joined,
    caseBody: stmt.caseBody,
    operation: stmt.operation,
    ...(index > 0 ? questions(stmts[index - 1]!, stmt) : {}),
  });
}

function dump(file: string, layout: Layout): string[] {
  const decoded = decode(readFileSync(file));
  if (typeof decoded !== "string") return [JSON.stringify({ file, skipped: decoded.message })];

  const text = withoutMark(decoded);
  const parsed = languageOf(file).parse(file, text);
  const doc: Doc = document(file, text, parsed.comments);
  const rejected = parsed.rejection(text);
  if (rejected) return [JSON.stringify({ file, skipped: rejected.message })];

  const lists = parsed.scan(doc, layout).lists.toSorted((left, right) => left.start - right.start);
  return lists.flatMap((list, at) =>
    list.stmts.map((_, index) => line(file, at, index, list.stmts)),
  );
}

if (import.meta.main) {
  const { values, positionals } = parseArgs({
    allowPositionals: true,
    options: {
      width: { type: "string", default: String(FALLBACK_WIDTH.columns) },
      tab: { type: "string", default: String(FALLBACK_WIDTH.tab) },
    },
  });

  const layout = { columns: Number(values.width), tab: Number(values.tab) };
  for (const file of positionals) for (const out of dump(file, layout)) console.log(out);
}
