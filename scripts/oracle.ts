import { extname } from "node:path";
import {
  parseSync,
  visitorKeys,
  type BlockStatement,
  type Comment,
  type Node,
  type ParserOptions,
} from "oxc-parser";

export type { Comment };

export interface Region {
  start: number;
  end: number;
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

type Statement = { node: Node; parent: Node | null };

const JAVASCRIPT: Record<string, ParserOptions> = {
  ".js": { lang: "jsx" },
  ".mjs": { lang: "jsx", sourceType: "module" },
  ".cjs": { lang: "jsx", sourceType: "commonjs" },
};

const POSITION_KEYS = new Set(["start", "end", "range", "loc"]);

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

function isNode(value: unknown): value is Node {
  return (
    typeof value === "object" &&
    value !== null &&
    "type" in value &&
    typeof value.type === "string" &&
    "start" in value &&
    typeof value.start === "number" &&
    "end" in value &&
    typeof value.end === "number"
  );
}

function children(node: Node): [string, Node][] {
  const fields = node as unknown as Record<string, unknown>;
  const keys = visitorKeys[node.type] ?? Object.keys(fields).filter((key) => key !== "parent");
  const result: [string, Node][] = [];
  for (const key of keys) {
    const value = fields[key];
    for (const child of Array.isArray(value) ? value : [value])
      if (isNode(child)) result.push([key, child]);
  }

  return result;
}

function walk(root: Node, enter: (node: Node, parent: Node | null) => void): void {
  const stack: Statement[] = [{ node: root, parent: null }];
  while (stack.length > 0) {
    const { node, parent } = stack.pop()!;
    enter(node, parent);

    const below = children(node);
    for (let index = below.length - 1; index >= 0; index--)
      stack.push({ node: below[index]![1], parent: node });
  }
}

function controlledBlocks(root: Node): BlockStatement[] {
  const blocks: BlockStatement[] = [];
  walk(root, (node) => {
    switch (node.type) {
      case "IfStatement":
        if (node.consequent.type === "BlockStatement") blocks.push(node.consequent);
        if (node.alternate?.type === "BlockStatement") blocks.push(node.alternate);
        return;

      case "ForStatement":
      case "ForInStatement":
      case "ForOfStatement":
      case "WhileStatement":
      case "DoWhileStatement":
        if (node.body.type === "BlockStatement") blocks.push(node.body);
    }
  });

  return blocks;
}

function container(program: Node, comment: Comment): Region {
  let enclosing: Node = program;
  walk(program, (node) => {
    if (ENCLOSING.has(node.type) && node.start < comment.start && comment.end <= node.end)
      if (node.end - node.start < enclosing.end - enclosing.start) enclosing = node;
  });

  return { start: enclosing.start, end: enclosing.end };
}

function controlledRanges(node: Node): Region[] {
  const ranges: Region[] = [];
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

      case "IfStatement": {
        const { consequent, alternate } = current;
        body(consequent);
        if (alternate) (alternate.type === "IfStatement" ? chain : body)(alternate);
        break;
      }

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

export function oracleSide(path: string, text: string): OracleSide {
  const parsed = parseSync(path, text, JAVASCRIPT[extname(path)]);
  const blocks = controlledBlocks(parsed.program);
  const flattened = new Set<unknown>(blocks.filter((block) => block.body.length === 1));

  return {
    comments: parsed.comments,
    blocks: blocks.map((block) => ({
      start: block.start,
      end: block.end,
      single: block.body.length === 1,
    })),
    shape: () =>
      parsed.errors.length > 0
        ? undefined
        : JSON.stringify(parsed.program, (key, value: unknown) => {
            if (POSITION_KEYS.has(key)) return undefined;
            if (typeof value === "bigint") return `${value}n`;
            return flattened.has(value) ? (value as BlockStatement).body[0] : value;
          }),
    container: (comment) => container(parsed.program, comment),
    statementAfter: (offset) => statementAfter(parsed.program, text, offset),
  };
}
