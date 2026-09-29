import type { BlockStatement, Node, Statement } from "oxc-parser";
import type { BracePass, BraceScan } from "../language.ts";
import { JUMP_TYPES, LOOP_TYPES, walk } from "./ast.ts";
import { commentIndex, finding, lineAt, nextToken, source } from "../../engine/doc.ts";
import type { OffsetEdit } from "../../engine/edits.ts";
import type { Comment, Doc } from "../../engine/model.ts";
import type { Finding } from "../../engine/types.ts";

const REMOVABLE = new Set(["ExpressionStatement", ...JUMP_TYPES, "IfStatement", ...LOOP_TYPES]);

function addOwned(owners: Map<BlockStatement, Node>, node: Node): void {
  switch (node.type) {
    case "IfStatement":
      if (node.consequent.type === "BlockStatement") owners.set(node.consequent, node);
      if (node.alternate?.type === "BlockStatement") owners.set(node.alternate, node);
      return;

    case "ForStatement":
    case "ForInStatement":
    case "ForOfStatement":
    case "WhileStatement":
    case "DoWhileStatement":
      if (node.body.type === "BlockStatement") owners.set(node.body, node);
  }
}

export function controlledBlocks(
  root: Node,
  enter: (node: Node, parent: Node | null) => void = () => {},
): Map<BlockStatement, Node> {
  const owners = new Map<BlockStatement, Node>();
  walk(root, enter, (node) => addOwned(owners, node));
  return owners;
}

function endsWithOpenIf(node: Statement, removed: ReadonlySet<BlockStatement>): boolean {
  let current = node;
  while (true) {
    switch (current.type) {
      case "IfStatement":
        if (!current.alternate) return true;
        current = current.alternate;
        continue;

      case "ForStatement":
      case "ForInStatement":
      case "ForOfStatement":
      case "WhileStatement":
      case "DoWhileStatement":
      case "LabeledStatement":
      case "WithStatement":
        current = current.body;
        continue;

      case "BlockStatement":
        if (!removed.has(current)) return false;
        current = current.body[0]!;
        continue;

      default:
        return false;
    }
  }
}

function openingEdit(doc: Doc, offset: number): OffsetEdit {
  const line = lineAt(doc, offset);
  const start = doc.lineStarts[line - 1]!;
  const end = start + doc.lines[line - 1]!.length;
  if (doc.lines[line - 1]!.trim() === "{") return { start, end: end + 1 };
  if (doc.text.slice(offset + 1, end).trim() === "") {
    const horizontal = /[^\S\r\n]*$/.exec(doc.text.slice(start, offset))![0].length;
    return { start: offset - horizontal, end: offset + 1 };
  }

  return { start: offset, end: offset + (doc.text[offset + 1] === " " ? 2 : 1) };
}

function closingEdit(doc: Doc, offset: number): OffsetEdit {
  const line = lineAt(doc, offset);
  const start = doc.lineStarts[line - 1]!;
  const end = start + doc.lines[line - 1]!.length;
  if (doc.lines[line - 1]!.trim() === "}") {
    const finalLine = end === doc.text.length;
    return { start, end: finalLine ? end : end + 1 };
  }

  if (/^ \w/.test(doc.text.slice(offset + 1))) return { start: offset, end: offset + 2 };
  return { start: offset - (doc.text[offset - 1] === " " ? 1 : 0), end: offset + 1 };
}

function continuation(doc: Doc, inner: Statement, block: BlockStatement): number | null {
  if (source(doc, inner).endsWith(";")) return null;

  const after = nextToken(doc, block.end);
  const separated = /[\r\n]/.test(doc.text.slice(block.end, after));
  const safe = after >= doc.text.length || (separated && !/^[([`+\-/.<;]/.test(doc.text[after]!));
  return safe ? null : after;
}

function fusesIdentifiers(doc: Doc, edit: OffsetEdit): boolean {
  return (
    /[$\\\p{ID_Continue}\u200C\u200D]$/u.test(
      doc.text.slice(Math.max(0, edit.start - 2), edit.start),
    ) && /^[$\\\p{ID_Continue}\u200C\u200D]/u.test(doc.text.slice(edit.end, edit.end + 2))
  );
}

export type BraceHold =
  | { kind: "count"; count: number }
  | { kind: "statement"; inner: Statement }
  | { kind: "continues"; inner: Statement; after: number }
  | { kind: "comment"; comment: Comment }
  | { kind: "closing"; comment: Comment }
  | { kind: "else"; inner: Statement }
  | { kind: "fuse" };

export function braceHold(
  doc: Doc,
  block: BlockStatement,
  removed: ReadonlySet<BlockStatement>,
  chained = false,
): BraceHold | null {
  const inner = block.body[0];
  if (block.body.length !== 1 || !inner) return { kind: "count", count: block.body.length };
  if (!REMOVABLE.has(inner.type)) return { kind: "statement", inner };

  const after = chained ? null : continuation(doc, inner, block);
  if (after !== null) return { kind: "continues", inner, after };

  for (
    let index = commentIndex(doc, block.start + 1);
    index < commentIndex(doc, block.end);
    index++
  ) {
    const comment = doc.comments[index]!;
    if (comment.start < inner.start || comment.end > inner.end) return { kind: "comment", comment };
  }

  const comment = doc.comments[commentIndex(doc, block.end)];
  const line = lineAt(doc, block.end - 1);
  if (
    comment &&
    doc.text.slice(block.end, comment.start).trim() === "" &&
    lineAt(doc, comment.start) === line &&
    doc.text.slice(doc.lineStarts[line - 1], block.end - 1).trim() === ""
  )
    return { kind: "closing", comment };

  const next = nextToken(doc, block.end);
  if (/^else\b/.test(doc.text.slice(next)) && endsWithOpenIf(inner, removed))
    return { kind: "else", inner };

  const opening = openingEdit(doc, block.start);
  const closing = closingEdit(doc, block.end - 1);
  if (fusesIdentifiers(doc, opening) || fusesIdentifiers(doc, closing)) return { kind: "fuse" };

  return null;
}

function trailingBody(statement: Statement): Statement | null {
  switch (statement.type) {
    case "IfStatement":
      return statement.alternate ? null : statement.consequent;

    case "ForStatement":
    case "ForInStatement":
    case "ForOfStatement":
    case "WhileStatement":
      return statement.body;

    default:
      return null;
  }
}

function closersEndAtFixedBrace(
  doc: Doc,
  from: number,
  byClosing: ReadonlyMap<number, BlockStatement>,
  known: Map<number, boolean>,
): boolean {
  const visited: number[] = [];

  let at = from;
  let answer: boolean | undefined;
  while (answer === undefined) {
    while (at < doc.text.length && /\s/.test(doc.text[at]!)) at++;
    answer = known.get(at);
    if (answer !== undefined) break;

    if (at >= doc.text.length || (doc.text[at] === "}" && !byClosing.has(at))) answer = true;
    else if (doc.text[at] !== "}") answer = false;
    else visited.push(at++);
  }

  for (const offset of visited) known.set(offset, answer);
  return answer;
}

function closesChain(
  doc: Doc,
  block: BlockStatement,
  removed: ReadonlySet<BlockStatement>,
  outerGoes: boolean,
  settled: () => boolean,
  codeStart: (offset: number) => number,
): boolean {
  const inner = block.body.length === 1 ? trailingBody(block.body[0]!) : null;
  if (inner?.type !== "BlockStatement" || !removed.has(inner)) return false;

  return (
    doc.text.slice(inner.end, block.end - 1) === " " &&
    codeStart(inner.end - 1) < inner.end - 1 &&
    doc.text.slice(block.end, block.end + 2) === " }" &&
    !outerGoes &&
    settled() &&
    braceHold(doc, block, removed, true) === null
  );
}

export function braceEdits(doc: Doc, blocks: BlockStatement[], collapseChains: boolean): BracePass {
  const edits: OffsetEdit[] = [];
  const findings: Finding[] = [];
  const removed = new Set<BlockStatement>();
  const remove = (block: BlockStatement) => {
    removed.add(block);
    edits.push(openingEdit(doc, block.start), closingEdit(doc, block.end - 1));
    findings.push(finding(doc, block.start, "braces"));
  };

  for (const block of blocks) if (braceHold(doc, block, removed) === null) remove(block);

  if (!collapseChains) return { edits, findings };

  const standard = new Set(removed);
  const byClosing = new Map(blocks.map((block) => [block.end - 1, block]));
  const known = new Map<number, boolean>();
  const codeStarts = new Map<number, number>();
  const codeStart = (offset: number) => {
    const line = doc.lineStarts[lineAt(doc, offset) - 1]!;

    let start = codeStarts.get(line);
    if (start === undefined) {
      const code = /[^\s{}]|\n/g;
      code.lastIndex = line;

      const match = code.exec(doc.text);
      start = match && match[0] !== "\n" ? match.index : Infinity;
      codeStarts.set(line, start);
    }

    return start;
  };

  for (const block of blocks) {
    const outer = byClosing.get(block.end + 1);
    const outerGoes = outer !== undefined && standard.has(outer);
    const settled = () => closersEndAtFixedBrace(doc, block.end, byClosing, known);
    if (!standard.has(block) && closesChain(doc, block, removed, outerGoes, settled, codeStart))
      remove(block);
  }

  return { edits, findings };
}

function statementName(doc: Doc, inner: Statement): string {
  if (inner.type === "BlockStatement") return "a nested block";
  if (inner.type === "EmptyStatement") return "an empty statement";
  if (inner.type === "LabeledStatement") return "a labeled statement";

  const word = /^[\w$]+/.exec(source(doc, inner))?.[0];
  if (word) return `a \`${word}\` statement`;

  return `a ${inner.type.replaceAll(/(?<=[a-z])(?=[A-Z])/g, " ").toLowerCase()}`;
}

function holdReason(
  doc: Doc,
  block: BlockStatement,
  hold: BraceHold,
  at: (offset: number) => number,
): string {
  switch (hold.kind) {
    case "count":
      return hold.count === 0 ? "the body is empty" : `the body holds ${hold.count} statements`;

    case "statement":
      return `the body is ${statementName(doc, hold.inner)}, and only an expression, \`return\`, \`throw\`, \`break\`, \`continue\`, \`if\` or loop stands without braces`;

    case "continues": {
      const line = lineAt(doc, hold.after);
      const end = doc.lineStarts[line - 1]! + doc.lines[line - 1]!.length;
      const where =
        line === lineAt(doc, block.end - 1) ? "the same line" : `line ${at(hold.after)}`;

      return `\`${source(doc, hold.inner).split("\n")[0]!.trim()}\` has no semicolon, so without braces it could continue onto \`${doc.text.slice(hold.after, end).trim()}\` on ${where}`;
    }

    case "comment":
      return `a comment on line ${at(hold.comment.start)} sits inside the braces but outside the statement`;

    case "closing":
      return `a comment after the closing brace on line ${at(hold.comment.start)} would move onto the next line without it`;

    case "else":
      return `without braces, the \`else\` on line ${at(nextToken(doc, block.end))} would attach to the \`if\` inside them`;

    case "fuse":
      return "removing the braces would run two words together";
  }
}

export function braceScan(
  doc: Doc,
  blocks: BlockStatement[],
  owners: Map<BlockStatement, Node>,
  chains: Map<Node, Node>,
): BraceScan {
  return {
    pass(touches, touched, collapseChains) {
      const selected = new Set<Node>();
      for (const stmt of touched) {
        let node = stmt.node as Node;
        while (node.type === "LabeledStatement") node = node.body;
        selected.add(node);
      }

      return braceEdits(
        doc,
        blocks.filter((block) => {
          const owner = owners.get(block)!;
          return (
            touches(lineAt(doc, block.start), lineAt(doc, block.end - 1)) ||
            selected.has(chains.get(owner) ?? owner)
          );
        }),
        collapseChains,
      );
    },
    opening(line) {
      const opening = [...owners.keys()].filter((block) => lineAt(doc, block.start) === line);
      const selected =
        opening.length > 0
          ? opening
          : [...owners]
              .filter(([, owner]) => lineAt(doc, owner.start) === line)
              .map(([block]) => block);

      return selected.map((block) => {
        const [inner] = block.body;
        return {
          start: block.start,
          open: lineAt(doc, block.start),
          close: lineAt(doc, block.end - 1),
          inner:
            inner && block.body.length === 1
              ? [lineAt(doc, inner.start), lineAt(doc, inner.end - 1)]
              : null,
        };
      });
    },
    hold(start, at) {
      const survivor = [...owners.keys()].find((candidate) => candidate.start === start);
      if (!survivor || !blocks.includes(survivor)) return { kind: "directive" };

      const hold = braceHold(doc, survivor, new Set());
      if (hold?.kind === "count")
        return { kind: "inapplicable", reason: holdReason(doc, survivor, hold, at) };

      return { kind: "held", reason: hold ? holdReason(doc, survivor, hold, at) : null };
    },
  };
}
