import { dlopen, FFIType } from "bun:ffi";

const LOCK_WAIT = 10_000;
const LOCK_EX = 2;
const LOCK_NB = 4;
let libc: { flock(descriptor: number, operation: number): number } | undefined;

function flock(descriptor: number, operation: number): boolean {
  libc ??= dlopen(process.platform === "darwin" ? "libc.dylib" : "libc.so.6", {
    flock: { args: [FFIType.i32, FFIType.i32], returns: FFIType.i32 },
  }).symbols;

  return libc.flock(descriptor, operation) === 0;
}

export { flock, LOCK_EX, LOCK_NB, LOCK_WAIT };
