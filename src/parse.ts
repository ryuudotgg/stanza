import {
  parseSync,
  type Comment,
  type OxcError,
  type ParserOptions,
  type Program,
} from "oxc-parser";
import { extname } from "node:path";

export interface Parsed {
  program: Program;
  comments: Comment[];
  errors: OxcError[];
}

const javascript: Record<string, ParserOptions> = {
  ".js": { lang: "jsx" },
  ".mjs": { lang: "jsx", sourceType: "module" },
  ".cjs": { lang: "jsx", sourceType: "commonjs" },
};

export function parse(path: string, text: string): Parsed {
  const result = parseSync(path, text, javascript[extname(path)]);
  return { program: result.program, comments: result.comments, errors: result.errors };
}
