declare module "oxc-parser/src-js/bindings.js" {
  import type { ParserOptions } from "oxc-parser";

  export function parseRawSync(
    path: string,
    block: Uint8Array,
    sourceStartPos: number,
    sourceByteLen: number,
    options: ParserOptions,
  ): void;
}

declare module "oxc-parser/src-js/raw-transfer/common.js" {
  export interface RawBuffer extends Uint8Array {
    block: Uint8Array;
    int32: Int32Array;
    float64: Float64Array;
  }

  export function prepareRaw(text: string): {
    buffer: RawBuffer;
    sourceStartPos: number;
    sourceByteLen: number;
  };
  export function isJsAst(buffer: RawBuffer): boolean;
  export function returnBufferToCache(buffer: RawBuffer): void;
}

declare module "oxc-parser/src-js/generated/deserialize/js.js" {
  const path: string;
  export default path;
}

declare module "oxc-parser/src-js/generated/deserialize/ts.js" {
  const path: string;
  export default path;
}
