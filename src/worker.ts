import { workerData } from "node:worker_threads";
import { formatFile, readText, type FileOutcome } from "./format.ts";
import { embedDeserializers } from "./languages/javascript/deserializers.ts";
import { deepRisk } from "./depth.ts";
import { CHUNK_SIZE, hardLinked, type ChunkMessage, type PoolData } from "./pool.ts";

const data = workerData as PoolData;
const counter = new Int32Array(data.counter);
const signal = new Int32Array(data.signal);
const claims = new Int32Array(data.claims);
const finished = new Int32Array(data.finished);
function post(message: ChunkMessage): void {
  data.port.postMessage(message);
  Atomics.add(signal, 0, 1);
  Atomics.notify(signal, 0);
}

try {
  if (data.addon !== undefined) process.env.NAPI_RS_NATIVE_LIBRARY_PATH = data.addon;
  embedDeserializers(data.deserializers);

  const chunkCount = Math.ceil(data.inputs.length / CHUNK_SIZE);
  for (
    let chunk = Atomics.add(counter, 0, 1);
    chunk < chunkCount;
    chunk = Atomics.add(counter, 0, 1)
  ) {
    Atomics.store(claims, chunk, data.id + 1);

    try {
      const warnings = { unread: new Set<string>(), unreadWidth: new Set<string>() };
      const outcomes: (FileOutcome | null)[] = [];
      for (const input of data.inputs.slice(chunk * CHUNK_SIZE, (chunk + 1) * CHUNK_SIZE)) {
        if (hardLinked(input, data.run)) {
          outcomes.push(null);
          continue;
        }

        const text = readText(input.path);
        if (typeof text === "string" && deepRisk(text, !/\.[cm]?ts$/.test(input.path))) {
          outcomes.push(null);
          continue;
        }

        outcomes.push(formatFile({ ...input, read: () => text }, data.run, input.output, warnings));
      }

      post({
        chunk,
        outcomes,
        unread: [...warnings.unread],
        unreadWidth: [...warnings.unreadWidth],
      });
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
