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

type ParseFn = (path: string, text: string) => Parsed;

async function embeddedParser(): Promise<ParseFn> {
  const bindingPath = (await import("./binding-embedded.ts")).default;
  const binding: Binding = createRequire(import.meta.url)(bindingPath);
  return (path, text) => {
    const raw = binding.parseSync(path, text);
    return { program: JSON.parse(raw.program).node, comments: raw.comments, errors: raw.errors };
  };
}

async function packageParser(): Promise<ParseFn> {
  const { parseSync } = await import("oxc-parser");
  return (path, text) => {
    const result = parseSync(path, text);
    return { program: result.program, comments: result.comments, errors: result.errors };
  };
}

const compiled = import.meta.path.startsWith("/$bunfs/");
export const parse: ParseFn = compiled ? await embeddedParser() : await packageParser();
