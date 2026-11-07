import { availableParallelism } from "node:os";
import { DEFAULT_WORKERS, workerCount } from "../../src/pool.ts";

if (import.meta.main) {
  const args = process.argv.slice(2);
  const cores = args[0] === "--cores" ? Number(args[1]) : availableParallelism();
  for (const count of args[0] === "--cores" ? args.slice(2) : args) {
    const files = Number(count);
    const workers = workerCount(files, process.env, DEFAULT_WORKERS, cores);
    console.log(JSON.stringify({ files, workers }));
  }
}
