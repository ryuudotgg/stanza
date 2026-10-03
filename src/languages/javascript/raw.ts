import type { Comment, OxcError, ParserOptions, Program } from "oxc-parser";
import { getBufferOffset, parseRawSync, rawTransferSupported } from "oxc-parser/src-js/bindings.js";
import {
  ACTIVE_SIZE,
  BLOCK_ALIGN,
  BLOCK_SIZE,
  BUFFER_SIZE,
  IS_TS_FLAG_POS,
} from "oxc-parser/src-js/generated/constants.js";
import { deserializerFiles } from "./deserializers.ts";
import type { Parsed } from "./parse.ts";

interface RawBuffer extends Uint8Array {
  block: Uint8Array;
  int32: Int32Array;
  float64: Float64Array;
}

interface Deserializer {
  deserialize(
    buffer: RawBuffer,
    text: string,
    sourceStartPos: number,
    sourceByteLen: number,
  ): { program: Program; comments: Comment[]; errors: OxcError[] };
}

const ALLOCATION_ATTEMPTS = 8;
const MAX_SOURCE_BYTES = 1 << 30;

if (!rawTransferSupported()) throw new Error("raw transfer needs a 64-bit little-endian platform");

// Required by path, not by literal, so a compiled binary does not parse 330 KB of deserializers on every start.
const files = deserializerFiles();
const deserializeJs = (require(files.js) as Deserializer).deserialize;
const deserializeTs = (require(files.ts) as Deserializer).deserialize;

const encoder = new TextEncoder();

let shared: RawBuffer | undefined;
export function parseRaw(path: string, text: string, options?: ParserOptions): Parsed {
  shared ??= createBuffer();

  const { sourceStartPos, sourceByteLen } = writeSource(shared, text);
  parseRawSync(path, shared.block, sourceStartPos, sourceByteLen, options ?? {});

  const isJs = shared[IS_TS_FLAG_POS] === 0;
  const deserialize = isJs ? deserializeJs : deserializeTs;
  const { program, comments, errors } = deserialize(shared, text, sourceStartPos, sourceByteLen);
  if (isJs && program.hashbang !== null) {
    const { value, start, end } = program.hashbang;
    comments.unshift({ type: "Line", value, start, end });
  }

  return { program, comments, errors };
}

function writeSource(
  buffer: RawBuffer,
  text: string,
): { sourceStartPos: number; sourceByteLen: number } {
  const maxBytes = text.length * 3;
  if (maxBytes > MAX_SOURCE_BYTES) throw new Error("Source text is too long for raw transfer");

  const sourceStartPos = ACTIVE_SIZE - maxBytes;
  const target = new Uint8Array(buffer.buffer, buffer.byteOffset + sourceStartPos, maxBytes);
  const { read, written } = encoder.encodeInto(text, target);
  if (read !== text.length) throw new Error("Failed to write source text into the raw buffer");

  return { sourceStartPos, sourceByteLen: written };
}

function createBuffer(): RawBuffer {
  const { arrayBuffer, offset } = allocate();

  const buffer = new Uint8Array(arrayBuffer, offset, BUFFER_SIZE) as RawBuffer;
  buffer.int32 = new Int32Array(arrayBuffer, offset, BUFFER_SIZE / 4);
  buffer.float64 = new Float64Array(arrayBuffer, offset, BUFFER_SIZE / 8);
  buffer.block = new Uint8Array(arrayBuffer, offset, BLOCK_SIZE);
  return buffer;
}

// Bun caps an ArrayBuffer at 4 GiB, so the aligned block only fits some starts; the live spacer shifts each retry by 2 GiB.
function allocate(): { arrayBuffer: ArrayBuffer; offset: number } {
  const spacers: ArrayBuffer[] = [];
  for (let attempt = 0; attempt < ALLOCATION_ATTEMPTS; attempt++) {
    const arrayBuffer = new ArrayBuffer(BLOCK_ALIGN);
    const offset = getBufferOffset(new Uint8Array(arrayBuffer));
    if (offset + BLOCK_SIZE <= BLOCK_ALIGN) return { arrayBuffer, offset };

    spacers.push(new ArrayBuffer(BLOCK_ALIGN / 2));
  }

  throw new Error("Failed to allocate a raw transfer buffer aligned on 4 GiB");
}
