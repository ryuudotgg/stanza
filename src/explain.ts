import { dirname, extname } from "node:path";
import type { BlockStatement, Node, Statement } from "oxc-parser";
import { walk } from "./ast.ts";
import { addControlledBlocks, braceEdits, braceHold, type BraceHold } from "./braces.ts";
import { braceDecisions } from "./config/index.ts";
import { within, type Region } from "./directives.ts";
import { document, lineAt, nextToken, source } from "./doc.ts";
import { applyOffsets, type OffsetEdit } from "./edits.ts";
import { blockSpacing, listGaps, matches, type Ruled } from "./gaps.ts";
import { scan, type Scan } from "./index.ts";
import type { Doc, Gap, StatementList } from "./model.ts";
import { parse } from "./parse.ts";
import { RULES } from "./rules.ts";

export interface ExplainRequest {
  path: string;
  text: string;
  line: number;
  noBraces: boolean;
  display: (path: string) => string;
}

export type Explanation = { found: boolean; lines: string[] } | { error: string };

interface Trace {
  original: Doc;
  frozen: Region[];
  doc: Doc;
  scanned: Scan;
  passes: OffsetEdit[][];
  unbraced: number[];
}

const LABEL = 11;

function field(label: string, text: string): string {
  return `${label ? `${label}:` : ""}`.padEnd(LABEL) + text;
}

function plural(count: number, noun: string): string {
  return `${count === 0 ? "no" : count} ${noun}${count === 1 ? "" : "s"}`;
}

function lineList(lines: number[]): string {
  const words = lines.map(String);
  const last = words.pop()!;
  return words.length === 0 ? `line ${last}` : `lines ${words.join(", ")} and ${last}`;
}

function shift(edits: OffsetEdit[], offset: number): number | null {
  let removed = 0;
  for (const edit of edits) {
    if (offset < edit.start) break;
    if (offset < edit.end) return null;
    removed += edit.end - edit.start;
  }

  return offset - removed;
}

function unshift(edits: OffsetEdit[], offset: number): number {
  let removed = 0;
  for (const edit of edits) {
    if (edit.start - removed > offset) break;
    removed += edit.end - edit.start;
  }

  return offset + removed;
}

function back(passes: OffsetEdit[][], offset: number): number {
  return passes.reduceRight((current, edits) => unshift(edits, current), offset);
}

function trace(original: Doc, keepBraces: boolean): Trace {
  const passes: OffsetEdit[][] = [];
  const unbraced: number[] = [];
  const first = scan(original);

  let doc = original;
  let scanned = first;
  for (
    let edits = keepBraces ? [] : braceEdits(doc, scanned.blocks).edits;
    edits.length > 0;
    edits = braceEdits(doc, scanned.blocks).edits
  ) {
    for (let index = 0; index < edits.length; index += 2)
      unbraced.push(lineAt(original, back(passes, edits[index]!.start)));

    const text = applyOffsets(doc.text, [...edits]);
    passes.push(edits.toSorted((left, right) => left.start - right.start));
    doc = document(doc.path, text, parse(doc.path, text));
    scanned = scan(doc);
  }

  return { original, frozen: first.frozen, doc, scanned, passes, unbraced };
}

function originalLine(trace: Trace, offset: number): number {
  return lineAt(trace.original, back(trace.passes, offset));
}

function excerpt(trace: Trace, first: number, last: number): string {
  const label = first === last ? `${first}` : `${first}-${last}`;
  return `  ${label.padEnd(LABEL - 2)}${trace.original.lines[first - 1]!.trim()}`;
}

function wants(decision: Ruled): string {
  return decision.want === "none" ? "no blank line" : "a blank line";
}

function jumpWord(doc: Doc, guard: Node): string {
  if (guard.type !== "IfStatement") return "a jump";

  const body =
    guard.consequent.type === "BlockStatement" ? guard.consequent.body[0] : guard.consequent;

  return body ? `\`${/^\w+/.exec(source(doc, body))![0]}\`` : "a jump";
}

function because(
  trace: Trace,
  list: StatementList,
  gap: Gap,
  decision: Ruled,
  at: (line: number) => number,
): string {
  const { prev, next } = gap;
  if ("name" in decision)
    return `\`${decision.name}\` is bound on line ${at(prev.codeStartLine)} and read by the \`${decision.reader}\` below it`;

  switch (decision.rule) {
    case "short-body":
      return `the block holds ${list.stmts.length} statements, each on one line`;

    case "guard-chain":
      return "both statements are single-line guards";

    case "after-multiline":
      return `the statement above spans lines ${at(prev.codeStartLine)} to ${at(prev.endLine)}`;

    case "switch-clauses":
      return "the clause above has a body";

    case "after-guard":
      return `the guard on line ${at(prev.codeStartLine)} ends in ${jumpWord(trace.doc, prev.node)}, so the next statement starts a new step`;

    case "let-step":
      return `the \`let\` on line ${at(next.codeStartLine)} joins the block below it, so it starts its own step`;
  }
}

function gapResult(gap: Gap, at: (line: number) => number): string {
  const { prev, next, blank, decision } = gap;
  if (decision.want === "keep" || decision.want === "frozen") return "--fix leaves the gap alone";
  if (next.detached)
    return `the comment above line ${at(next.codeStartLine)} is set apart by a blank line, so --fix leaves the gap alone`;
  if (next.startLine <= prev.endLine)
    return "both statements share a line, so --fix leaves the gap alone";

  if (decision.want === "none")
    return blank === 0
      ? "already has no blank line"
      : `--fix removes ${plural(blank, "blank line")}`;

  return blank > 0 ? "already has a blank line" : "--fix adds a blank line";
}

function explainGap(trace: Trace, list: StatementList & Region, gap: Gap): string[] {
  const { prev, next, decision } = gap;
  const at = (line: number) => originalLine(trace, trace.doc.lineStarts[line - 1]!);
  const lines = [
    `gap above line ${at(next.codeStartLine)}, ${plural(gap.blank, "blank line")}`,
    excerpt(trace, at(prev.codeStartLine), at(prev.endLine)),
    excerpt(trace, at(next.codeStartLine), at(next.codeStartLine)),
  ];

  const first = originalLine(trace, list.start);
  const last = originalLine(trace, list.end - 1);
  const unbraced = trace.unbraced.filter((line) => first <= line && line <= last);
  if (unbraced.length > 0)
    lines.push(field("note", `decided after --fix removes the braces on ${lineList(unbraced)}`));

  if (decision.want === "frozen")
    lines.push(field("rule", "none, a stanza directive covers one of the two statements"));
  else if (decision.want === "keep")
    lines.push(field("rule", "none applies, the blank lines stay as written"));
  else {
    const outranked = [...matches(trace.doc, list, prev, next)].filter(
      (match) => match.rule !== decision.rule,
    );

    lines.push(
      field("rule", `${decision.rule}, wants ${wants(decision)}`),
      field("because", because(trace, list, gap, decision, at)),
      ...(outranked.length === 0
        ? [field("outranked", "none")]
        : outranked.map((match, index) =>
            field(
              index === 0 ? "outranked" : "",
              `${match.rule}, wants ${wants(match)}: ${because(trace, list, gap, match, at)}`,
            ),
          )),
    );
  }

  lines.push(field("result", gapResult(gap, at)));

  const [reported] = blockSpacing(trace.doc, gap);
  if (reported) lines.push(field("reported", `block-spacing, ${reported.message}`));

  return lines;
}

function statementName(doc: Doc, inner: Statement): string {
  if (inner.type === "BlockStatement") return "a nested block";
  if (inner.type === "EmptyStatement") return "an empty statement";
  if (inner.type === "LabeledStatement") return "a labeled statement";

  const word = /^[\w$]+/.exec(source(doc, inner))?.[0];
  if (word) return `a \`${word}\` statement`;

  return `a ${inner.type.replaceAll(/(?<=[a-z])(?=[A-Z])/g, " ").toLowerCase()}`;
}

function holdReason(trace: Trace, block: BlockStatement, hold: BraceHold): string {
  const { doc } = trace;
  switch (hold.kind) {
    case "count":
      return hold.count === 0 ? "the body is empty" : `the body holds ${hold.count} statements`;

    case "statement":
      return `the body is ${statementName(doc, hold.inner)}, and only an expression, \`return\`, \`throw\`, \`break\`, \`continue\`, \`if\` or loop stands without braces`;

    case "continues": {
      const line = lineAt(doc, hold.after);
      const end = doc.lineStarts[line - 1]! + doc.lines[line - 1]!.length;
      const where =
        line === lineAt(doc, block.end - 1)
          ? "the same line"
          : `line ${originalLine(trace, hold.after)}`;

      return `\`${source(doc, hold.inner).split("\n")[0]!.trim()}\` has no semicolon, so without braces it could continue onto \`${doc.text.slice(hold.after, end).trim()}\` on ${where}`;
    }

    case "comment":
      return `a comment on line ${originalLine(trace, hold.comment.start)} sits inside the braces but outside the statement`;

    case "else":
      return `without braces, the \`else\` on line ${originalLine(trace, nextToken(doc, block.end))} would attach to the \`if\` inside them`;

    case "fuse":
      return "removing the braces would run two words together";
  }
}

function configHolds(request: ExplainRequest): string[] {
  if (request.noBraces) return ["--no-braces turns the rule off"];

  try {
    return braceDecisions(dirname(request.path), extname(request.path))
      .filter((decision) => decision.setting !== "off")
      .map((decision) => {
        const files = decision.files.map(request.display).join(", ");
        return decision.setting === "on"
          ? `${files} enforces braces`
          : `stanza cannot tell whether ${files} enforces braces, so it keeps them`;
      });
  } catch (error: unknown) {
    return [`reading the lint config failed, so stanza keeps them: ${String(error)}`];
  }
}

function controlledBlocks(doc: Doc): Map<BlockStatement, Node> {
  const owners = new Map<BlockStatement, Node>();
  walk(
    doc.program,
    () => {},
    (node) => {
      const blocks: BlockStatement[] = [];
      addControlledBlocks(node, blocks);
      for (const block of blocks) owners.set(block, node);
    },
  );

  return owners;
}

function blocksAt(doc: Doc, line: number): BlockStatement[] {
  const owners = controlledBlocks(doc);
  const opening = [...owners.keys()].filter((block) => lineAt(doc, block.start) === line);
  if (opening.length > 0) return opening;

  return [...owners]
    .filter(([, owner]) => lineAt(doc, owner.start) === line)
    .map(([block]) => block);
}

function explainBlock(trace: Trace, block: BlockStatement, config: string[]): string[] {
  const { original } = trace;
  const open = lineAt(original, block.start);
  const close = lineAt(original, block.end - 1);
  const lines = [
    open === close ? `braced body on line ${open}` : `braced body, lines ${open} to ${close}`,
  ];

  const [inner] = block.body;
  if (inner && block.body.length === 1)
    lines.push(excerpt(trace, lineAt(original, inner.start), lineAt(original, inner.end - 1)));

  const rule = field("rule", `braces, ${RULES.braces.summary}`);

  let offset: number | null = block.start;
  for (const [pass, edits] of trace.passes.entries()) {
    offset = shift(edits, offset);
    if (offset === null)
      return [
        ...lines,
        rule,
        field(
          "result",
          pass === 0
            ? "--fix removes the braces"
            : "--fix removes the braces once the braces inside them are gone",
        ),
      ];
  }

  const survivor = [...controlledBlocks(trace.doc).keys()].find(
    (candidate) => candidate.start === offset,
  );

  if (!survivor || !trace.scanned.blocks.includes(survivor))
    return [...lines, field("rule", "braces, but a stanza directive covers this body")];

  const hold = braceHold(trace.doc, survivor, new Set());
  if (hold?.kind === "count")
    return [...lines, field("rule", `braces does not apply, ${holdReason(trace, survivor, hold)}`)];

  const reasons = [...config, ...(hold ? [holdReason(trace, survivor, hold)] : [])];
  return [
    ...lines,
    rule,
    ...reasons.map((reason, index) => field(index === 0 ? "kept by" : "", reason)),
    field("result", reasons.length > 0 ? "the braces stay" : "--fix removes the braces"),
  ];
}

export function explain(request: ExplainRequest): Explanation {
  const { path, text, line } = request;
  const parsed = parse(path, text);
  const original = document(path, text, parsed);
  const error = parsed.errors[0];
  if (error) {
    const at = lineAt(original, error.labels[0]?.start ?? 0);
    return { error: `${request.display(path)}:${at} does not parse: ${error.message}` };
  }

  if (line > original.lines.length)
    return { error: `${request.display(path)} has ${original.lines.length} lines, not ${line}` };

  const config = configHolds(request);
  const traced = trace(original, config.length > 0);
  const sections: string[][] = [];
  for (const list of traced.scanned.lists)
    for (const gap of listGaps(traced.doc, list)) {
      const starts = [gap.next.codeStartLine, gap.next.startLine].map((number) =>
        originalLine(traced, traced.doc.lineStarts[number - 1]!),
      );

      if (starts.includes(line)) sections.push(explainGap(traced, list, gap));
    }

  for (const block of blocksAt(original, line)) sections.push(explainBlock(traced, block, config));

  if (sections.length === 0 && within(traced.frozen, original.lineStarts[line - 1]!))
    sections.push([`line ${line} is inside a stanza-off region, so stanza leaves it alone`]);

  const header = `${request.display(path)}:${line}`;
  if (sections.length === 0)
    return {
      found: false,
      lines: [`${header}: no gap ends and no braced body starts on this line`],
    };

  return { found: true, lines: [header, ...sections.flatMap((section) => ["", ...section])] };
}
