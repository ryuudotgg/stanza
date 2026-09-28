import {
  parseSync,
  type Comment,
  type Node,
  type OxcError,
  type ParserOptions,
  type Program,
} from "oxc-parser";
import { extname } from "node:path";
import { walk } from "./ast.ts";

export interface Parsed {
  program: Program;
  comments: Comment[];
  errors: OxcError[];
}

export interface Rejection {
  start: number;
  message: string;
}

const javascript: Record<string, ParserOptions> = {
  ".js": { lang: "jsx" },
  ".mjs": { lang: "jsx", sourceType: "module" },
  ".cjs": { lang: "jsx", sourceType: "commonjs" },
};

const loneCr = /\r(?!\n)/g;

export function parse(path: string, text: string): Parsed {
  const result = parseSync(path, text, javascript[extname(path)]);
  return { program: result.program, comments: result.comments, errors: result.errors };
}

function crLineEnding(text: string, program: Program): number | undefined {
  if (text.search(loneCr) < 0) return undefined;

  const quasis: Node[] = [];
  walk(program, (node) => {
    if (node.type === "TemplateElement") quasis.push(node);
  });

  quasis.sort((a, b) => a.start - b.start);

  let index = 0;
  for (const { index: offset } of text.matchAll(loneCr)) {
    while (index < quasis.length && quasis[index]!.end <= offset) index++;
    if (index === quasis.length || offset < quasis[index]!.start) return offset;
  }

  return undefined;
}

export function rejection(text: string, parsed: Parsed): Rejection | undefined {
  const error = parsed.errors[0];
  if (error) return { start: error.labels[0]?.start ?? 0, message: error.message };

  const cr = crLineEnding(text, parsed.program);
  if (cr !== undefined)
    return { start: cr, message: "CR line endings are not supported, left untouched" };
}

export function parseErrors(path: string, text: string): OxcError[] {
  return parseSync(path, text, javascript[extname(path)]).errors;
}
