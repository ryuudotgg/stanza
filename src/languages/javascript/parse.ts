import {
  parseSync,
  type Comment,
  type Node,
  type OxcError,
  type ParserOptions,
  type Program,
} from "oxc-parser";
import { extname } from "node:path";
import type { Rejection } from "../language.ts";
import { walk } from "./ast.ts";

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

const loneCr = /\r(?!\n)/g;

const RAW_AFTER_LENGTH = 500_000;
const RAW_MAX_LENGTH = 1_000_000;
const RAW_COLLECT_EVERY = 1_000_000;

let parsedSinceCollect = 0;

type Handoff =
  | { kind: "pending"; remaining: number; required: boolean }
  | { kind: "raw"; parseRaw: typeof import("./raw.ts").parseRaw }
  | { kind: "json" }
  | { kind: "unavailable"; error: Error };

let handoff: Handoff | undefined;

export function parse(path: string, text: string): Parsed {
  const options = javascript[extname(path)];
  const current = currentHandoff(text.length);
  if (current.kind === "unavailable") throw current.error;

  if (current.kind === "raw") {
    collectWhileRaw(text.length);

    if (text.length <= RAW_MAX_LENGTH)
      try {
        return current.parseRaw(path, text, options);
      } catch {
        // The raw deserializer recurses and overflows the stack on deep or long inputs that JSON.parse handles.
      }
  }

  const result = parseSync(path, text, options);
  return { program: result.program, comments: result.comments, errors: result.errors };
}

function collectWhileRaw(length: number): void {
  parsedSinceCollect += length;
  if (parsedSinceCollect < RAW_COLLECT_EVERY) return;

  parsedSinceCollect = 0;
  // JSC counts the 4 GiB raw transfer buffer as live heap, so on its own it never collects the ASTs a whole repo run leaves behind.
  Bun.gc(true);
}

function currentHandoff(length: number): Handoff {
  handoff ??= initialHandoff();
  if (handoff.kind === "pending" && (handoff.remaining -= length) <= 0)
    handoff = loadRaw(handoff.required);

  return handoff;
}

function initialHandoff(): Handoff {
  const mode = process.env.STANZA_RAW_TRANSFER;
  if (mode === "0") return { kind: "json" };
  return {
    kind: "pending",
    remaining: mode === "1" ? 0 : RAW_AFTER_LENGTH,
    required: mode === "1",
  };
}

function loadRaw(required: boolean): Handoff {
  try {
    // A bare require is the one lazy load bun build --compile embeds; createRequire and import.meta.require fail at runtime in the binary.
    const { parseRaw } = require("./raw.ts") as typeof import("./raw.ts");
    parseRaw("probe.js", "", javascript[".js"]);
    return { kind: "raw", parseRaw };
  } catch (cause) {
    if (!required) return { kind: "json" };

    const error = new Error(`STANZA_RAW_TRANSFER=1 but raw transfer is unavailable: ${cause}`, {
      cause,
    });

    return { kind: "unavailable", error };
  }
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
