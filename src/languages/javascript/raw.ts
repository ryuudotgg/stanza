import type { Comment, OxcError, ParserOptions, Program } from "oxc-parser";
import { parseRawSync } from "oxc-parser/src-js/bindings.js";
import {
  isJsAst,
  prepareRaw,
  returnBufferToCache,
  type RawBuffer,
} from "oxc-parser/src-js/raw-transfer/common.js";
import { deserializerFiles } from "./deserializers.ts";
import type { Parsed } from "./parse.ts";

interface Deserializer {
  deserialize(
    buffer: RawBuffer,
    text: string,
    sourceStartPos: number,
    sourceByteLen: number,
  ): { program: Program; comments: Comment[]; errors: OxcError[] };
}

// Required by path, not by literal, so a compiled binary does not parse 330 KB of deserializers on every start.
const files = deserializerFiles();
const deserializeJs = (require(files.js) as Deserializer).deserialize;
const deserializeTs = (require(files.ts) as Deserializer).deserialize;
export function parseRaw(path: string, text: string, options?: ParserOptions): Parsed {
  const { buffer, sourceStartPos, sourceByteLen } = prepareRaw(text);
  try {
    parseRawSync(path, buffer.block, sourceStartPos, sourceByteLen, options ?? {});

    const isJs = isJsAst(buffer);
    const deserialize = isJs ? deserializeJs : deserializeTs;
    const { program, comments, errors } = deserialize(buffer, text, sourceStartPos, sourceByteLen);
    if (isJs && program.hashbang !== null) {
      const { value, start, end } = program.hashbang;
      comments.unshift({ type: "Line", value, start, end });
    }

    return { program, comments, errors };
  } finally {
    returnBufferToCache(buffer);
  }
}
