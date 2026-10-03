declare module "oxc-parser/src-js/bindings.js" {
  import type { ParserOptions } from "oxc-parser";

  export function getBufferOffset(buffer: Uint8Array): number;
  export function rawTransferSupported(): boolean;
  export function parseRawSync(
    path: string,
    block: Uint8Array,
    sourceStartPos: number,
    sourceByteLen: number,
    options: ParserOptions,
  ): void;
}

declare module "oxc-parser/src-js/generated/constants.js" {
  export const ACTIVE_SIZE: number;
  export const BLOCK_ALIGN: number;
  export const BLOCK_SIZE: number;
  export const BUFFER_SIZE: number;
  export const IS_TS_FLAG_POS: number;
}

declare module "oxc-parser/src-js/generated/deserialize/js.js" {
  const path: string;
  export default path;
}

declare module "oxc-parser/src-js/generated/deserialize/ts.js" {
  const path: string;
  export default path;
}
