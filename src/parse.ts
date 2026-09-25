import { parseSync, type Comment, type OxcError, type Program } from "oxc-parser";

export interface Parsed {
  program: Program;
  comments: Comment[];
  errors: OxcError[];
}

export function parse(path: string, text: string): Parsed {
  const result = parseSync(path, text);
  return { program: result.program, comments: result.comments, errors: result.errors };
}
