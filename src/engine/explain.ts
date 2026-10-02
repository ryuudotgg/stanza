import { dirname, extname } from "node:path";
import { FALLBACK_WIDTH, type Language, type Opening, type Width } from "../languages/language.ts";
import { within } from "./directives.ts";
import { document, lineAt } from "./doc.ts";
import { offsetMap, type OffsetMap } from "./edits.ts";
import { blockSpacing, listGaps, matches, type Ruled } from "./gaps.ts";
import { traceFix, type Trace } from "./index.ts";
import type { Doc, Gap, StatementList, Stmt } from "./model.ts";
import { RULES } from "./rules.ts";
import type { Braces } from "./types.ts";

export interface ExplainRequest {
  language: Language;
  path: string;
  text: string;
  line: number;
  braces: Braces | undefined;
  display: (path: string) => string;
}

export type Explanation = { found: boolean; lines: string[] } | { error: string };

interface Traced extends Trace {
  width: Width;
  display: (path: string) => string;
  maps: OffsetMap[];
  unbraced: number[];
  excerpts: Map<string, string>;
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

function back(maps: OffsetMap[], offset: number): number {
  return maps.reduceRight((current, map) => map.back(current), offset);
}

function originalLine(trace: Traced, offset: number): number {
  return lineAt(trace.original, back(trace.maps, offset));
}

function excerpt(trace: Traced, first: number, last: number): string {
  const label = first === last ? `${first}` : `${first}-${last}`;
  const cached = trace.excerpts.get(label);
  if (cached !== undefined) return cached;

  const line = `  ${label.padEnd(LABEL - 2)}${trace.original.lines[first - 1]!.trim()}`;
  trace.excerpts.set(label, line);
  return line;
}

function wants(decision: Ruled): string {
  return decision.want === "none" ? "no blank line" : "a blank line";
}

function codeLine(doc: Doc, stmt: Stmt): number {
  return lineAt(doc, stmt.start);
}

function because(
  trace: Traced,
  list: StatementList,
  gap: Gap,
  decision: Ruled,
  at: (line: number) => number,
): string {
  const { prev, next } = gap;
  if ("name" in decision)
    return `\`${decision.name}\` is bound on line ${at(codeLine(trace.doc, prev))} and read by the \`${decision.reader}\` below it`;

  switch (decision.rule) {
    case "short-body":
      return `the block holds ${list.stmts.length} statements, each compact`;

    case "guard-chain":
      return prev.joined !== null || next.joined !== null
        ? `both guards fit on one line at ${trace.width.columns} columns`
        : "both statements are compact guards";

    case "after-multiline":
      return `the statement above spans lines ${at(codeLine(trace.doc, prev))} to ${at(prev.endLine)}`;

    case "switch-clauses":
      return "the clause above has a body";

    case "after-guard":
      return `the guard on line ${at(codeLine(trace.doc, prev))} ends in ${prev.guard?.jump ? `\`${prev.guard.jump}\`` : "a jump"}, so the next statement starts a new step`;

    case "let-step":
      return `the \`${next.declaration?.keyword}\` on line ${at(codeLine(trace.doc, next))} joins the block below it, so it starts its own step`;
  }
}

function gapResult(trace: Traced, gap: Gap, at: (line: number) => number): string {
  const { prev, next, blank, decision } = gap;
  if (decision.want === "keep" || decision.want === "frozen") return "--fix leaves the gap alone";
  if (next.detached)
    return `the comment above line ${at(codeLine(trace.doc, next))} is set apart by a blank line, so --fix leaves the gap alone`;

  if (next.startLine <= prev.endLine)
    return "both statements share a line, so --fix leaves the gap alone";

  if (decision.want === "none")
    return blank === 0
      ? "already has no blank line"
      : `--fix removes ${plural(blank, "blank line")}`;

  return blank > 0 ? "already has a blank line" : "--fix adds a blank line";
}

function explainGap(trace: Traced, list: StatementList, gap: Gap): string[] {
  const { prev, next, decision } = gap;
  const at = (line: number) => originalLine(trace, trace.doc.lineStarts[line - 1]!);
  const lines = [
    `gap above line ${at(codeLine(trace.doc, next))}, ${plural(gap.blank, "blank line")}`,
    excerpt(trace, at(codeLine(trace.doc, prev)), at(prev.endLine)),
    excerpt(trace, at(codeLine(trace.doc, next)), at(codeLine(trace.doc, next))),
  ];

  const first = originalLine(trace, list.start);
  const last = originalLine(trace, list.end - 1);
  const unbraced = trace.unbraced.filter((line) => first <= line && line <= last);
  if (unbraced.length > 0)
    lines.push(field("note", `decided after --fix removes the braces on ${lineList(unbraced)}`));

  const joined = [prev, next].filter((stmt) => stmt.joined !== null);
  if (joined.length > 0) {
    const source = trace.width.source;
    const label =
      source.kind === "config"
        ? trace.display(source.file)
        : source.kind === "default"
          ? `${source.formatter} default`
          : "fallback";

    lines.push(field("width", `${trace.width.columns} columns, from ${label}`));

    for (const stmt of joined)
      lines.push(
        field(
          "",
          `line ${at(codeLine(trace.doc, stmt))} joins to ${stmt.joined} columns, ${stmt.joined! <= trace.width.columns ? "fits on one line" : "too long for one line"}`,
        ),
      );
  }

  if (decision.want === "frozen")
    lines.push(field("rule", "none, a stanza directive covers one of the two statements"));
  else if (decision.want === "keep")
    lines.push(field("rule", "none applies, the blank lines stay as written"));
  else {
    const outranked = [...matches(list, prev, next)].filter(
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

  lines.push(field("result", gapResult(trace, gap, at)));

  const [reported] = blockSpacing(trace.doc, gap);
  if (reported) lines.push(field("reported", `block-spacing, ${reported.message}`));

  return lines;
}

function configHolds(language: Language, request: Omit<ExplainRequest, "line">): string[] {
  if (request.braces === "off") return ["--no-braces turns the rule off"];
  if (request.braces === "on") return [];

  try {
    return (language.config?.decisions(dirname(request.path), extname(request.path)) ?? [])
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

function explainBlock(trace: Traced, block: Opening, config: string[]): string[] {
  const { open, close, inner } = block;
  const lines = [
    open === close ? `braced body on line ${open}` : `braced body, lines ${open} to ${close}`,
  ];

  if (inner) lines.push(excerpt(trace, ...inner));

  const rule = field("rule", `braces, ${RULES.braces.summary}`);

  let offset = block.start;
  for (const [pass, map] of trace.maps.entries()) {
    if (map.removes(offset))
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

    offset = map.forward(offset);
  }

  const hold = trace.scanned.braces!.hold(offset, (moved) => originalLine(trace, moved));
  if (hold.kind === "directive")
    return [...lines, field("rule", "braces, but a stanza directive covers this body")];

  if (hold.kind === "inapplicable")
    return [...lines, field("rule", `braces does not apply, ${hold.reason}`)];

  const reasons = [...config, ...(hold.reason ? [hold.reason] : [])];
  return [
    ...lines,
    rule,
    ...reasons.map((reason, index) => field(index === 0 ? "kept by" : "", reason)),
    field("result", reasons.length > 0 ? "the braces stay" : "--fix removes the braces"),
  ];
}

export function explainer(
  request: Omit<ExplainRequest, "line">,
): { error: string } | ((line: number) => Explanation) {
  const { language, path, text } = request;
  const parsed = language.parse(path, text);
  const original = document(path, text, parsed.comments);
  const rejected = parsed.rejection(text);
  if (rejected) {
    const at = lineAt(original, rejected.start);
    return { error: `${request.display(path)}:${at} does not parse: ${rejected.message}` };
  }

  const config = configHolds(language, request);
  const width = language.config?.width(path) ?? FALLBACK_WIDTH;

  const fixed = traceFix(language, original, parsed, config.length > 0, width);
  const maps = fixed.passes.map(offsetMap);
  const unbraced: number[] = [];
  for (const [pass, edits] of fixed.passes.entries())
    for (let index = 0; index < edits.length; index += 2)
      unbraced.push(lineAt(original, back(maps.slice(0, pass), edits[index]!.start)));

  const traced: Traced = {
    ...fixed,
    width,
    display: request.display,
    maps,
    unbraced,
    excerpts: new Map(),
  };

  const gapsAt = new Map<number, [StatementList, Gap][]>();
  for (const list of traced.scanned.lists)
    for (const gap of listGaps(traced.doc, list)) {
      const starts = [codeLine(traced.doc, gap.next), gap.next.startLine].map((number) =>
        originalLine(traced, traced.doc.lineStarts[number - 1]!),
      );

      for (const start of new Set(starts))
        gapsAt.set(start, [...(gapsAt.get(start) ?? []), [list, gap]]);
    }

  return (line: number): Explanation => {
    if (line > original.lines.length)
      return { error: `${request.display(path)} has ${original.lines.length} lines, not ${line}` };

    const sections = (gapsAt.get(line) ?? []).map(([list, gap]) => explainGap(traced, list, gap));
    for (const block of traced.first.braces?.opening(line) ?? [])
      sections.push(explainBlock(traced, block, config));

    if (sections.length === 0 && within(traced.first.frozen, original.lineStarts[line - 1]!))
      sections.push([`line ${line} is inside a stanza-off region, so stanza leaves it alone`]);

    const header = `${request.display(path)}:${line}`;
    if (sections.length === 0)
      return {
        found: false,
        lines: [`${header}: no gap ends and no braced body starts on this line`],
      };

    const unread =
      width.unread.length > 0
        ? [
            "",
            field(
              "note",
              `could not read the line width from ${width.unread.map(request.display).join(", ")}`,
            ),
          ]
        : [];

    return {
      found: true,
      lines: [header, ...unread, ...sections.flatMap((section) => ["", ...section])],
    };
  };
}

export function explain(request: ExplainRequest): Explanation {
  const result = explainer(request);
  return typeof result === "function" ? result(request.line) : result;
}
