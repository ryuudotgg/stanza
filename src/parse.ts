import { createRequire } from "node:module";
import type { Comment, OxcError, Program } from "oxc-parser";

export interface Parsed {
  program: Program;
  comments: Comment[];
  errors: OxcError[];
}

interface Binding {
  parseSync(
    path: string,
    text: string,
    options?: undefined,
  ): { program: string; comments: Comment[]; errors: OxcError[] };
}

const require = createRequire(import.meta.url);
const compiled = import.meta.path.startsWith("/$bunfs/");
const bindingPath = compiled
  ? (await import("./binding-embedded.ts")).default
  : require.resolve("@oxc-parser/binding-darwin-arm64");

const binding: Binding = require(bindingPath);
export function parse(path: string, text: string): Parsed {
  const raw = binding.parseSync(path, text);
  const program: Program = JSON.parse(raw.program).node;
  return { program, comments: raw.comments, errors: raw.errors };
}
