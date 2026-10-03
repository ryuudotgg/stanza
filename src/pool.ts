import { availableParallelism } from "node:os";
import { statSync } from "node:fs";
import { workerData, type MessagePort } from "node:worker_threads";
import {
  formatFile,
  mergeOutcomes,
  readText,
  type FileOutcome,
  type FormatRun,
  type Formatted,
  type Input,
} from "./format.ts";
import { deepRisk } from "./depth.ts";
import {
  deserializerFiles,
  embedDeserializers,
  type DeserializerFiles,
} from "./languages/javascript/deserializers.ts";

export const THRESHOLD = 256;
export const DEFAULT_WORKERS = 3;
export const CHUNK_SIZE = 64;

export interface FileInput extends Omit<Input, "read"> {
  output: string;
}

export interface PoolData<Input, Job> {
  id: number;
  inputs: Input[];
  job: Job;
  chunkSize: number;
  counter: SharedArrayBuffer;
  signal: SharedArrayBuffer;
  claims: SharedArrayBuffer;
  finished: SharedArrayBuffer;
  port: MessagePort;
  addon: string | undefined;
  deserializers: DeserializerFiles;
}

export type ChunkMessage<Result> =
  | (Result & { chunk: number })
  | { chunk: number | undefined; error: string };

export interface PoolTask<Input, Outcome, Result extends { outcomes: (Outcome | null)[] }, Job> {
  worker: string | URL;
  job: Job;
  chunkSize?: number;
  chunk(inputs: Input[]): Result;
  merge(result: Result): void;
  item(input: Input): Outcome;
}

export function workerCount(
  fileCount: number,
  env: Readonly<Record<string, string | undefined>>,
  maximum = DEFAULT_WORKERS,
): number {
  const override = env.STANZA_WORKERS;
  if (override !== undefined && /^\d+$/.test(override)) {
    const count = Number(override);
    if (count <= 64) return count;
  }

  if (fileCount < THRESHOLD) return 0;

  return Math.max(0, Math.min(maximum, availableParallelism() - 1));
}

export function hardLinked(input: FileInput, run: FormatRun): boolean {
  if (run.mode !== "fix" || !run.write) return false;

  try {
    return statSync(input.path).nlink > 1;
  } catch {
    return false;
  }
}

export function tooDeep(path: string, text: string): boolean {
  return deepRisk(text, !/\.[cm]?ts$/.test(path));
}

export function formatChunk(inputs: FileInput[], run: FormatRun, inWorker: boolean) {
  const warnings = { unread: new Set<string>(), unreadWidth: new Set<string>() };
  const outcomes = inputs.map((input) => {
    if (hardLinked(input, run)) return null;

    const text = readText(input.path);
    if (inWorker && typeof text === "string" && tooDeep(input.path, text)) return null;

    return formatFile({ ...input, read: () => text }, run, input.output, warnings);
  });

  return { outcomes, unread: [...warnings.unread], unreadWidth: [...warnings.unreadWidth] };
}

function acceptChunk<Input, Outcome, Result extends { outcomes: (Outcome | null)[] }, Job>(
  inputCount: number,
  chunkSize: number,
  message: ChunkMessage<Result>,
  chunks: (Outcome | null)[][],
  task: PoolTask<Input, Outcome, Result, Job>,
  errors: string[],
): void {
  if ("error" in message) {
    errors.push(message.error);
    if (message.chunk === undefined) return;

    const size = Math.min(chunkSize, inputCount - message.chunk * chunkSize);
    chunks[message.chunk] = Array<null>(size).fill(null);
    return;
  }

  chunks[message.chunk] = message.outcomes;
  task.merge(message);
}

function recoverAbandoned<Outcome>(
  inputCount: number,
  chunkSize: number,
  chunks: (Outcome | null)[][],
  claims: Int32Array,
  stopped: boolean[],
  errors: string[],
): number {
  let recovered = 0;
  for (let chunk = 0; chunk < claims.length; chunk++) {
    const owner = Atomics.load(claims, chunk);
    if (chunks[chunk] !== undefined || owner === 0 || !stopped[owner - 1]) continue;

    const size = Math.min(chunkSize, inputCount - chunk * chunkSize);
    chunks[chunk] = Array<null>(size).fill(null);
    errors.push("a worker thread stopped before reporting its files, so main formatted them");
    recovered++;
  }

  return recovered;
}

export function runPooled<Input, Outcome, Result extends { outcomes: (Outcome | null)[] }, Job>(
  inputs: Input[],
  task: PoolTask<Input, Outcome, Result, Job>,
  count: number,
): { outcomes: Outcome[]; errors: string[] } {
  const { Worker, MessageChannel, receiveMessageOnPort } =
    require("node:worker_threads") as typeof import("node:worker_threads");

  const counter = new Int32Array(new SharedArrayBuffer(4));
  const signal = new Int32Array(new SharedArrayBuffer(4));

  const chunkSize = task.chunkSize ?? CHUNK_SIZE;
  const chunkCount = Math.ceil(inputs.length / chunkSize);
  const claims = new Int32Array(new SharedArrayBuffer(4 * chunkCount));
  const finished = new Int32Array(new SharedArrayBuffer(4 * Math.max(count, 1)));
  const chunks: (Outcome | null)[][] = Array(chunkCount);

  const errors: string[] = [];
  const ports: MessagePort[] = [];
  const deserializers = deserializerFiles();
  for (let index = 0; index < count; index++) {
    const { port1, port2 } = new MessageChannel();
    try {
      const workerData: PoolData<Input, Job> = {
        id: index,
        inputs,
        job: task.job,
        chunkSize,
        counter: counter.buffer as SharedArrayBuffer,
        signal: signal.buffer as SharedArrayBuffer,
        claims: claims.buffer as SharedArrayBuffer,
        finished: finished.buffer as SharedArrayBuffer,
        port: port2,
        addon: process.env.NAPI_RS_NATIVE_LIBRARY_PATH,
        deserializers,
      };

      const worker = new Worker(task.worker, { workerData, transferList: [port2] });
      worker.on("error", (error: Error) => errors.push(error.message));
      worker.unref();
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
    const stopped = Array.from(finished, (_, index) => Atomics.load(finished, index) === 1);

    for (const port of ports) {
      let received: ReturnType<typeof receiveMessageOnPort>;
      while ((received = receiveMessageOnPort(port)) !== undefined) {
        const message = received.message as ChunkMessage<Result>;
        acceptChunk(inputs.length, chunkSize, message, chunks, task, errors);
        if (message.chunk !== undefined) completed++;
      }
    }

    const chunk = Atomics.add(counter, 0, 1);
    if (chunk < chunkCount) {
      try {
        const result = task.chunk(inputs.slice(chunk * chunkSize, (chunk + 1) * chunkSize));
        acceptChunk(inputs.length, chunkSize, { chunk, ...result }, chunks, task, errors);
      } catch (error: unknown) {
        acceptChunk(
          inputs.length,
          chunkSize,
          { chunk, error: error instanceof Error ? error.message : String(error) },
          chunks,
          task,
          errors,
        );
      }

      completed++;
      continue;
    }

    completed += recoverAbandoned(inputs.length, chunkSize, chunks, claims, stopped, errors);
    if (completed < chunkCount) Atomics.wait(signal, 0, observed, 50);
  }

  const outcomes: Outcome[] = [];
  for (let chunk = 0; chunk < chunks.length; chunk++)
    for (const [offset, outcome] of chunks[chunk]!.entries()) {
      const input = inputs[chunk * chunkSize + offset]!;
      outcomes.push(outcome ?? task.item(input));
    }

  for (const port of ports) port.close();

  return { outcomes, errors };
}

export function serveChunks<Input, Job, Result>(work: (inputs: Input[], job: Job) => Result): void {
  const data = workerData as PoolData<Input, Job>;
  const counter = new Int32Array(data.counter);
  const signal = new Int32Array(data.signal);
  const claims = new Int32Array(data.claims);
  const finished = new Int32Array(data.finished);
  function post(message: ChunkMessage<Result>): void {
    data.port.postMessage(message);
    Atomics.add(signal, 0, 1);
    Atomics.notify(signal, 0);
  }

  try {
    if (data.addon !== undefined) process.env.NAPI_RS_NATIVE_LIBRARY_PATH = data.addon;
    embedDeserializers(data.deserializers);

    const chunkCount = Math.ceil(data.inputs.length / data.chunkSize);
    for (
      let chunk = Atomics.add(counter, 0, 1);
      chunk < chunkCount;
      chunk = Atomics.add(counter, 0, 1)
    ) {
      Atomics.store(claims, chunk, data.id + 1);

      try {
        const result = work(
          data.inputs.slice(chunk * data.chunkSize, (chunk + 1) * data.chunkSize),
          data.job,
        );

        post({ chunk, ...result });
      } catch (error: unknown) {
        post({ chunk, error: error instanceof Error ? error.message : String(error) });
      }
    }
  } catch (error: unknown) {
    post({ chunk: undefined, error: error instanceof Error ? error.message : String(error) });
  } finally {
    Atomics.store(finished, data.id, 1);
    Atomics.add(signal, 0, 1);
    Atomics.notify(signal, 0);
  }
}

export function formatPooled(
  inputs: FileInput[],
  run: FormatRun,
  count: number,
): { formatted: Formatted; errors: string[] } {
  const warnings = { unread: new Set<string>(), unreadWidth: new Set<string>() };
  // A compiled Bun worker resolves a bare specifier inside bunfs, not a file URL.
  const specifier = Bun.main.includes("/$bunfs/")
    ? "./worker.ts"
    : new URL("./worker.ts", import.meta.url);

  const { outcomes, errors } = runPooled(
    inputs,
    {
      worker: specifier,
      job: run,
      chunk: (inputs) => formatChunk(inputs, run, false),
      merge(result) {
        for (const path of result.unread) warnings.unread.add(path);
        for (const path of result.unreadWidth) warnings.unreadWidth.add(path);
      },
      item: (input): FileOutcome =>
        formatFile({ ...input, read: () => readText(input.path) }, run, input.output, warnings),
    },
    count,
  );

  return { formatted: mergeOutcomes(outcomes, warnings), errors };
}
