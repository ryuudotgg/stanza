import { availableParallelism } from "node:os";
import { statSync } from "node:fs";
import type { MessagePort, Worker } from "node:worker_threads";
import {
  formatFile,
  mergeOutcomes,
  readText,
  type FileOutcome,
  type FormatRun,
  type Formatted,
  type Input,
  type Warnings,
} from "./format.ts";
import { deserializerFiles, type DeserializerFiles } from "./languages/javascript/deserializers.ts";

export const THRESHOLD = 256;
export const DEFAULT_WORKERS = 3;
export const CHUNK_SIZE = 64;

export interface FileInput extends Omit<Input, "read"> {
  output: string;
}

export interface PoolData {
  inputs: FileInput[];
  run: FormatRun;
  counter: SharedArrayBuffer;
  signal: SharedArrayBuffer;
  port: MessagePort;
  addon: string | undefined;
  deserializers: DeserializerFiles;
}

export type ChunkMessage =
  | {
      chunk: number;
      outcomes: (FileOutcome | null)[];
      unread: string[];
      unreadWidth: string[];
    }
  | { chunk: number | undefined; error: string };

export function workerCount(
  fileCount: number,
  env: Readonly<Record<string, string | undefined>>,
): number {
  const override = env.STANZA_WORKERS;
  if (override !== undefined && /^\d+$/.test(override)) {
    const count = Number(override);
    if (count <= 64) return count;
  }

  if (fileCount < THRESHOLD) return 0;

  return Math.max(0, Math.min(DEFAULT_WORKERS, availableParallelism() - 1));
}

export function hardLinked(input: FileInput, run: FormatRun): boolean {
  if (run.mode !== "fix" || !run.write) return false;

  try {
    return statSync(input.path).nlink > 1;
  } catch {
    return false;
  }
}

function localChunk(inputs: FileInput[], run: FormatRun, chunk: number): ChunkMessage {
  const warnings = { unread: new Set<string>(), unreadWidth: new Set<string>() };
  const outcomes = inputs
    .slice(chunk * CHUNK_SIZE, (chunk + 1) * CHUNK_SIZE)
    .map((input) =>
      hardLinked(input, run)
        ? null
        : formatFile({ ...input, read: () => readText(input.path) }, run, input.output, warnings),
    );

  return { chunk, outcomes, unread: [...warnings.unread], unreadWidth: [...warnings.unreadWidth] };
}

function acceptChunk(
  inputCount: number,
  message: ChunkMessage,
  chunks: (FileOutcome | null)[][],
  warnings: Warnings,
  errors: string[],
): void {
  if ("error" in message) {
    errors.push(message.error);
    if (message.chunk === undefined) return;

    const size = Math.min(CHUNK_SIZE, inputCount - message.chunk * CHUNK_SIZE);
    chunks[message.chunk] = Array<null>(size).fill(null);
    return;
  }

  chunks[message.chunk] = message.outcomes;
  for (const path of message.unread) warnings.unread.add(path);
  for (const path of message.unreadWidth) warnings.unreadWidth.add(path);
}

export function formatPooled(
  inputs: FileInput[],
  run: FormatRun,
  count: number,
): { formatted: Formatted; errors: string[] } {
  const { Worker, MessageChannel, receiveMessageOnPort } =
    require("node:worker_threads") as typeof import("node:worker_threads");

  const counter = new Int32Array(new SharedArrayBuffer(4));
  const signal = new Int32Array(new SharedArrayBuffer(4));
  const chunkCount = Math.ceil(inputs.length / CHUNK_SIZE);
  const chunks: (FileOutcome | null)[][] = Array(chunkCount);
  const warnings = { unread: new Set<string>(), unreadWidth: new Set<string>() };

  const errors: string[] = [];
  const workers: Worker[] = [];
  const ports: MessagePort[] = [];
  const deserializers = deserializerFiles();
  // A compiled Bun worker resolves a bare specifier inside bunfs, not a file URL.
  const specifier = import.meta.url.includes("/$bunfs/")
    ? "./worker.ts"
    : new URL("./worker.ts", import.meta.url);

  for (let index = 0; index < count; index++) {
    const { port1, port2 } = new MessageChannel();
    try {
      const workerData: PoolData = {
        inputs,
        run,
        counter: counter.buffer as SharedArrayBuffer,
        signal: signal.buffer as SharedArrayBuffer,
        port: port2,
        addon: process.env.NAPI_RS_NATIVE_LIBRARY_PATH,
        deserializers,
      };

      const worker = new Worker(specifier, { workerData, transferList: [port2] });
      worker.on("error", (error: Error) => errors.push(error.message));
      worker.unref();
      workers.push(worker);
      ports.push(port1);
    } catch (error: unknown) {
      errors.push(error instanceof Error ? error.message : String(error));
      port1.close();
      port2.close();
    }
  }

  let completed = 0;
  while (completed < chunkCount) {
    const observed = Atomics.load(signal, 0);

    for (const port of ports) {
      let received: ReturnType<typeof receiveMessageOnPort>;
      while ((received = receiveMessageOnPort(port)) !== undefined) {
        const message = received.message as ChunkMessage;
        acceptChunk(inputs.length, message, chunks, warnings, errors);
        if (message.chunk !== undefined) completed++;
      }
    }

    const chunk = Atomics.add(counter, 0, 1);
    if (chunk < chunkCount) {
      try {
        acceptChunk(inputs.length, localChunk(inputs, run, chunk), chunks, warnings, errors);
      } catch (error: unknown) {
        acceptChunk(
          inputs.length,
          { chunk, error: error instanceof Error ? error.message : String(error) },
          chunks,
          warnings,
          errors,
        );
      }

      completed++;
    } else if (completed < chunkCount) Atomics.wait(signal, 0, observed, 50);
  }

  const outcomes: FileOutcome[] = [];
  for (let chunk = 0; chunk < chunks.length; chunk++)
    for (const [offset, outcome] of chunks[chunk]!.entries()) {
      const input = inputs[chunk * CHUNK_SIZE + offset]!;
      outcomes.push(
        outcome ??
          formatFile({ ...input, read: () => readText(input.path) }, run, input.output, warnings),
      );
    }

  const formatted = mergeOutcomes(outcomes, warnings);
  for (const worker of workers) void worker.terminate();
  for (const port of ports) port.close();

  return { formatted, errors };
}
