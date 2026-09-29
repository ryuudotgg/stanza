import type { BlockStatement, Node } from "oxc-parser";
import type { Comment } from "../../engine/model.ts";
import type { Region } from "../../engine/directives.ts";
import type { Oracle, OracleStatement } from "../language.ts";
import { children, walk } from "./ast.ts";
import { controlledBlocks } from "./braces.ts";
import { parse, type Parsed } from "./parse.ts";

const POSITION_KEYS = new Set(["start", "end", "range", "loc"]);

type Range = { start: number; end: number };
type Statement = { node: Node; parent: Node | null };

const ENCLOSING = new Set([
  "BlockStatement",
  "StaticBlock",
  "SwitchStatement",
  "SwitchCase",
  "ClassBody",
  "TSModuleBlock",
]);

const INTERIORS = new Set(["BlockStatement", "StaticBlock", "ClassBody", "TSModuleBlock"]);

const STATEMENT_ARRAYS = new Set([
  "Program:body",
  "BlockStatement:body",
  "StaticBlock:body",
  "SwitchStatement:cases",
  "SwitchCase:consequent",
  "TSModuleBlock:body",
]);

function shape(parsed: Parsed, blocks: BlockStatement[]): string | undefined {
  if (parsed.errors.length > 0) return undefined;

  const flattened = new Set(blocks.filter((block) => block.body.length === 1));
  return JSON.stringify(parsed.program, (key, value) => {
    if (POSITION_KEYS.has(key)) return undefined;
    if (typeof value === "bigint") return `${value}n`;
    return flattened.has(value) ? value.body[0] : value;
  });
}

function container(program: Node, comment: Comment): Region {
  let enclosing: Node = program;
  walk(program, (node) => {
    if (ENCLOSING.has(node.type) && node.start < comment.start && comment.end <= node.end)
      if (node.end - node.start < enclosing.end - enclosing.start) enclosing = node;
  });

  return { start: enclosing.start, end: enclosing.end };
}

function controlledRanges(node: Node): Range[] {
  const ranges: Range[] = [];
  function body(child: Node): void {
    ranges.push(
      child.type === "BlockStatement"
        ? { start: child.start + 1, end: child.end - 1 }
        : { start: child.start, end: child.end },
    );
  }

  function chain(current: Node): void {
    switch (current.type) {
      case "LabeledStatement":
        chain(current.body);
        break;

      case "IfStatement":
        body(current.consequent);

        if (current.alternate?.type === "IfStatement") chain(current.alternate);
        else if (current.alternate) body(current.alternate);

        break;

      case "ForStatement":
      case "ForInStatement":
      case "ForOfStatement":
      case "WhileStatement":
      case "DoWhileStatement":
      case "WithStatement":
        body(current.body);
    }
  }

  chain(node);
  return ranges;
}

function statementSkeleton(text: string, statement: Statement): string {
  const ranges = controlledRanges(statement.node);

  walk(statement.node, (node) => {
    if (ranges.some((range) => range.start <= node.start && node.end <= range.end)) return;
    if (INTERIORS.has(node.type)) ranges.push({ start: node.start + 1, end: node.end - 1 });
    else if (node.type === "SwitchStatement")
      ranges.push({ start: node.discriminant.end, end: node.end - 1 });
  });

  const outermost = ranges
    .sort((left, right) => left.start - right.start || right.end - left.end)
    .filter(
      (range, index, all) =>
        !all.slice(0, index).some((prior) => prior.start <= range.start && range.end <= prior.end),
    );

  let cursor = statement.node.start;
  let result = "";
  for (const range of outermost) {
    result += text.slice(cursor, range.start) + "\0";
    cursor = range.end;
  }

  return result + text.slice(cursor, statement.node.end);
}

function edgeAbove(parent: Node): number | undefined {
  if (parent.type === "BlockStatement" || parent.type === "StaticBlock") return parent.start;
  if (parent.type === "SwitchStatement") return parent.discriminant.end;
}

function edgeBelow(parent: Node): number | undefined {
  if (["BlockStatement", "StaticBlock", "SwitchStatement"].includes(parent.type)) return parent.end;
}

function gaps(text: string, statement: Statement): [string | undefined, string | undefined] {
  if (!statement.parent) return [undefined, undefined];

  const entry = children(statement.parent).find(([, child]) => child === statement.node);
  if (!entry || !STATEMENT_ARRAYS.has(`${statement.parent.type}:${entry[0]}`))
    return [undefined, undefined];

  const siblings = children(statement.parent)
    .filter(([key]) => key === entry[0])
    .map(([, child]) => child);

  const index = siblings.indexOf(statement.node);
  const previous = siblings[index - 1];
  const next = siblings[index + 1];

  const above = previous?.end ?? edgeAbove(statement.parent);
  const below = next?.start ?? edgeBelow(statement.parent);
  return [
    above === undefined ? undefined : text.slice(above, statement.node.start),
    below === undefined ? undefined : text.slice(statement.node.end, below),
  ];
}

function statementAfter(program: Node, text: string, offset: number): OracleStatement | undefined {
  let next: Statement | undefined;
  walk(program, (node, parent) => {
    if (
      node.start < offset ||
      (node.type !== "SwitchCase" && !/(Statement|Declaration)$/.test(node.type))
    )
      return;

    if (!next || node.start < next.node.start) next = { node, parent };
  });

  if (!next) return undefined;

  const statement = next;
  return {
    start: statement.node.start,
    clause: statement.node.type === "SwitchCase",
    skeleton: () => statementSkeleton(text, statement),
    gaps: () => gaps(text, statement),
  };
}

export const oracle: Oracle = {
  side(path, text) {
    const parsed = parse(path, text);
    const blocks = [...controlledBlocks(parsed.program).keys()];
    return {
      comments: parsed.comments,
      blocks: blocks.map((block) => ({
        start: block.start,
        end: block.end,
        single: block.body.length === 1,
      })),
      shape: () => shape(parsed, blocks),
      container: (comment) => container(parsed.program, comment),
      statementAfter: (offset) => statementAfter(parsed.program, text, offset),
    };
  },
};
