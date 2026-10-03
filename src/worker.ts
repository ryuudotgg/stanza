import { formatChunk, serveChunks, type FileInput } from "./pool.ts";
import type { FormatRun } from "./format.ts";

serveChunks((inputs: FileInput[], run: FormatRun) => formatChunk(inputs, run, true));
