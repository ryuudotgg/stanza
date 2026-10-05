import { availableParallelism } from "node:os";
import { statSync } from "node:fs";
import type { MessagePort } from "node:worker_threads";
import {
  formatFile,
  mergeOutcomes,
  readText,
  type FileOutcome,
  type FormatRun,
  type Formatted,
  type Input,
} from "./format.ts";
import {
  deserializerFiles,
  embedDeserializers,
  type DeserializerFiles,
} from "./languages/javascript/deserializers.ts";

export const THRESHOLD = 256;
export const DEFAULT_WORKERS = 3;
export const CHUNK_SIZE = 64;

export const LANE = { starting: 0, serving: 1, finished: 2, gone: 3 } as const;
type LaneState = (typeof LANE)[keyof typeof LANE];

export type Deferral = (path: string, text: string) => boolean;

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
  lanes: SharedArrayBuffer;
  posted: SharedArrayBuffer;
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

interface Board {
  counter: Int32Array;
  signal: Int32Array;
  claims: Int32Array;
  lanes: Int32Array;
  posted: Int32Array;
}

interface Ledger<Outcome> {
  readonly inputCount: number;
  readonly chunkSize: number;
  readonly results: ((Outcome | null)[] | undefined)[];
  readonly errors: string[];
  pending: number;
  unowned: number;
}

interface Lane {
  readonly port: MessagePort;
  drained: number;
  started: boolean;
  fault: string | undefined;
  exitCode: number | undefined;
  recovered: number;
}

export function workerCount(
  fileCount: number,
  env: Readonly<Record<string, string | undefined>>,
  maximum = DEFAULT_WORKERS,
  cores = availableParallelism(),
): number {
  const override = env.STANZA_WORKERS;
  if (override !== undefined && /^\d+$/.test(override)) {
    const count = Number(override);
    if (count <= 64) return count;
  }

  if (fileCount < THRESHOLD) return 0;

  return Math.max(0, Math.min(maximum, cores - 1));
}

export function hardLinked(input: FileInput, run: FormatRun): boolean {
  if (run.mode !== "fix" || !run.write) return false;

  try {
    return statSync(input.path).nlink > 1;
  } catch {
    return false;
  }
}

export function formatChunk(inputs: FileInput[], run: FormatRun, defer?: Deferral) {
  const warnings = { unread: new Set<string>(), unreadWidth: new Set<string>() };
  const outcomes = inputs.map((input) => {
    if (hardLinked(input, run)) return null;

    const text = readText(input.path);
    if (defer !== undefined && typeof text === "string" && defer(input.path, text)) return null;

    return formatFile({ ...input, read: () => text }, run, input.output, warnings);
  });

  return { outcomes, unread: [...warnings.unread], unreadWidth: [...warnings.unreadWidth] };
}

function wake(signal: Int32Array): void {
  Atomics.add(signal, 0, 1);
  Atomics.notify(signal, 0);
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function chunkLength(ledger: Ledger<unknown>, chunk: number): number {
  return Math.min(ledger.chunkSize, ledger.inputCount - chunk * ledger.chunkSize);
}

function settle<Outcome>(
  ledger: Ledger<Outcome>,
  chunk: number,
  outcomes: (Outcome | null)[],
): boolean {
  if (ledger.results[chunk] !== undefined) return false;

  ledger.results[chunk] = outcomes;
  ledger.pending--;
  return true;
}

function settleOnMain(ledger: Ledger<unknown>, chunk: number): boolean {
  return settle(ledger, chunk, Array<null>(chunkLength(ledger, chunk)).fill(null));
}

function accept<Input, Outcome, Result extends { outcomes: (Outcome | null)[] }, Job>(
  ledger: Ledger<Outcome>,
  message: ChunkMessage<Result>,
  task: PoolTask<Input, Outcome, Result, Job>,
): void {
  if ("error" in message) {
    if (message.chunk === undefined || settleOnMain(ledger, message.chunk))
      ledger.errors.push(message.error);

    return;
  }

  if (settle(ledger, message.chunk, message.outcomes)) task.merge(message);
}

function stopped(states: LaneState[]): boolean {
  return states.every((state) => state >= LANE.finished);
}

function quiet(
  index: number,
  states: LaneState[],
  posted: number[],
  lanes: Map<number, Lane>,
): boolean {
  return states[index]! >= LANE.finished && (lanes.get(index)?.drained ?? 0) >= posted[index]!;
}

function recoverStopped(
  ledger: Ledger<unknown>,
  claims: Int32Array,
  states: LaneState[],
  posted: number[],
  lanes: Map<number, Lane>,
): void {
  const everyLaneQuiet = states.every((_, index) => quiet(index, states, posted, lanes));
  for (let chunk = 0; chunk < claims.length; chunk++) {
    const owner = Atomics.load(claims, chunk);
    const ownerQuiet = owner !== 0 && quiet(owner - 1, states, posted, lanes);
    if (!ownerQuiet && !everyLaneQuiet) continue;
    if (!settleOnMain(ledger, chunk)) continue;

    const lane = lanes.get(owner - 1);
    if (lane === undefined) ledger.unowned += chunkLength(ledger, chunk);
    else lane.recovered += chunkLength(ledger, chunk);
  }
}

function fileCount(count: number): string {
  return `${count} ${count === 1 ? "file" : "files"}`;
}

function laneLoss(lane: Lane): string | undefined {
  const exit = `exited with code ${lane.exitCode}`;
  if (!lane.started)
    return `a worker thread failed to start, so main did its share: ${lane.fault ?? exit}`;

  if (lane.recovered === 0 && lane.fault === undefined && (lane.exitCode ?? 0) !== 0)
    return `a worker thread ${exit}, so main did its share`;

  if (lane.recovered === 0) return lane.fault;

  const files = fileCount(lane.recovered);
  if (lane.fault !== undefined)
    return `a worker thread failed before reporting ${files}, so main took them over: ${lane.fault}`;

  const ending = lane.exitCode === undefined ? "stopped" : exit;
  return `a worker thread ${ending} before reporting ${files}, so main took them over`;
}

function recoveredAny(ledger: Ledger<unknown>, lanes: Map<number, Lane>): boolean {
  return ledger.unowned > 0 || [...lanes.values()].some((lane) => lane.recovered > 0);
}

function losses(ledger: Ledger<unknown>, lanes: Map<number, Lane>): string[] {
  const laneLosses = [...lanes.values()].flatMap((lane) => laneLoss(lane) ?? []);
  const unowned =
    ledger.unowned === 0
      ? []
      : [
          `a worker thread stopped before reporting ${fileCount(ledger.unowned)}, so main took them over`,
        ];

  return [...ledger.errors, ...laneLosses, ...unowned];
}

export async function runPooled<
  Input,
  Outcome,
  Result extends { outcomes: (Outcome | null)[] },
  Job,
>(
  inputs: Input[],
  task: PoolTask<Input, Outcome, Result, Job>,
  count: number,
): Promise<{ outcomes: Outcome[]; errors: string[] }> {
  const { Worker, MessageChannel, receiveMessageOnPort } =
    require("node:worker_threads") as typeof import("node:worker_threads");

  const chunkSize = task.chunkSize ?? CHUNK_SIZE;
  const chunkCount = Math.ceil(inputs.length / chunkSize);
  const board: Board = {
    counter: new Int32Array(new SharedArrayBuffer(4)),
    signal: new Int32Array(new SharedArrayBuffer(4)),
    claims: new Int32Array(new SharedArrayBuffer(4 * chunkCount)),
    lanes: new Int32Array(new SharedArrayBuffer(4 * count)),
    posted: new Int32Array(new SharedArrayBuffer(4 * count)),
  };

  const ledger: Ledger<Outcome> = {
    inputCount: inputs.length,
    chunkSize,
    results: Array.from({ length: chunkCount }, () => undefined),
    errors: [],
    pending: chunkCount,
    unowned: 0,
  };

  const lanes = new Map<number, Lane>();
  const deserializers = deserializerFiles();
  let open = true;

  for (let id = 0; id < count; id++) {
    const { port1, port2 } = new MessageChannel();
    try {
      const workerData: PoolData<Input, Job> = {
        id,
        inputs,
        job: task.job,
        chunkSize,
        counter: board.counter.buffer as SharedArrayBuffer,
        signal: board.signal.buffer as SharedArrayBuffer,
        claims: board.claims.buffer as SharedArrayBuffer,
        lanes: board.lanes.buffer as SharedArrayBuffer,
        posted: board.posted.buffer as SharedArrayBuffer,
        port: port2,
        addon: process.env.NAPI_RS_NATIVE_LIBRARY_PATH,
        deserializers,
      };

      const worker = new Worker(task.worker, { workerData, transferList: [port2] });
      const lane: Lane = {
        port: port1,
        drained: 0,
        started: true,
        fault: undefined,
        exitCode: undefined,
        recovered: 0,
      };

      worker.on("error", (error: Error) => {
        if (!open) return;
        lane.fault ??= error.message;
        wake(board.signal);
      });

      worker.on("exit", (code: number) => {
        if (!open) return;

        lane.started = Atomics.load(board.lanes, id) !== LANE.starting;
        lane.exitCode = code;
        Atomics.store(board.lanes, id, LANE.gone);
        wake(board.signal);
      });

      worker.unref();
      lanes.set(id, lane);
    } catch (error: unknown) {
      Atomics.store(board.lanes, id, LANE.gone);
      ledger.errors.push(errorText(error));
      port1.close();
      port2.close();
    }
  }

  try {
    for (;;) {
      const observed = Atomics.load(board.signal, 0);
      const states = Array.from(
        board.lanes,
        (_, index) => Atomics.load(board.lanes, index) as LaneState,
      );

      const posted = Array.from(board.posted, (_, index) => Atomics.load(board.posted, index));

      for (const lane of lanes.values()) {
        let received: ReturnType<typeof receiveMessageOnPort>;
        while ((received = receiveMessageOnPort(lane.port)) !== undefined) {
          lane.drained++;
          accept(ledger, received.message as ChunkMessage<Result>, task);
        }
      }

      const chunk =
        Atomics.load(board.counter, 0) < chunkCount ? Atomics.add(board.counter, 0, 1) : chunkCount;

      if (chunk < chunkCount) {
        try {
          const result = task.chunk(inputs.slice(chunk * chunkSize, (chunk + 1) * chunkSize));
          accept(ledger, { chunk, ...result }, task);
        } catch (error: unknown) {
          accept(ledger, { chunk, error: errorText(error) }, task);
        }

        continue;
      }

      recoverStopped(ledger, board.claims, states, posted, lanes);

      const undelivered = [...lanes].some(([index, lane]) => lane.drained < posted[index]!);
      const exited = [...lanes.values()].every((lane) => lane.exitCode !== undefined);
      const settled = ledger.pending === 0 && stopped(states) && !undelivered;
      if (settled && (exited || !recoveredAny(ledger, lanes))) break;

      if (undelivered && stopped(states)) {
        await new Promise((resolve) => setImmediate(resolve));
        continue;
      }

      // Bun releases a waitAsync's hold on the event loop when another thread notifies, before main resumes.
      const holdLoop = setInterval(() => {}, 2 ** 30);
      try {
        await Atomics.waitAsync(board.signal, 0, observed).value;
      } finally {
        clearInterval(holdLoop);
      }
    }
  } finally {
    open = false;
    for (const lane of lanes.values()) lane.port.close();
  }

  const outcomes: Outcome[] = [];
  for (const [chunk, results] of ledger.results.entries())
    for (const [offset, outcome] of results!.entries())
      outcomes.push(outcome ?? task.item(inputs[chunk * chunkSize + offset]!));

  return { outcomes, errors: losses(ledger, lanes) };
}

export function serveChunks<Input, Job, Result>(work: (inputs: Input[], job: Job) => Result): void {
  const { workerData } = require("node:worker_threads") as typeof import("node:worker_threads");
  const data = workerData as PoolData<Input, Job>;

  const counter = new Int32Array(data.counter);
  const signal = new Int32Array(data.signal);
  const claims = new Int32Array(data.claims);
  const lanes = new Int32Array(data.lanes);
  const posted = new Int32Array(data.posted);
  function post(message: ChunkMessage<Result>): void {
    data.port.postMessage(message);
    Atomics.add(posted, data.id, 1);
    wake(signal);
  }

  Atomics.store(lanes, data.id, LANE.serving);
  wake(signal);

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
        post({ chunk, error: errorText(error) });
      }
    }
  } catch (error: unknown) {
    post({ chunk: undefined, error: errorText(error) });
  } finally {
    Atomics.store(lanes, data.id, LANE.finished);
    wake(signal);
  }
}

export async function formatPooled(
  inputs: FileInput[],
  run: FormatRun,
  count: number,
): Promise<{ formatted: Formatted; errors: string[] }> {
  const warnings = { unread: new Set<string>(), unreadWidth: new Set<string>() };
  // A compiled Bun worker resolves a bare specifier inside bunfs, not a file URL.
  const specifier = Bun.main.includes("/$bunfs/")
    ? "./worker.ts"
    : new URL("./worker.ts", import.meta.url);

  const { outcomes, errors } = await runPooled(
    inputs,
    {
      worker: specifier,
      job: run,
      chunk: (inputs) => formatChunk(inputs, run),
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
