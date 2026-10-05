import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export type WorkerRole =
  | "log"
  | "late-post"
  | "post-after-finish"
  | "setup-error-after-finish"
  | "load"
  | "exit-at-load"
  | "exit-after-serving"
  | "exit-on-post"
  | "throw-on-post"
  | "throw-once";

export interface PoolHarness {
  command: string[];
  workerChunks(): number;
}

export const HARD_EXIT_CODE = 9;
export const LOAD_FAILURE = "worker load failed";
export const CHUNK_FAILURE = "injected chunk failure";
export const POST_FAILURE = "clone failed";
export const SETUP_FAILURE = "worker setup failed";

export const HANG_LIMIT_MS = 10_000;
export const RUN_LIMIT_MS = 60_000;
const LATE_POST_MS = 300;
const HELD_SETUP_MS = 2000;
function roleSource(role: WorkerRole, worker: string, log: string): string {
  const importWorker = `await import(${JSON.stringify(worker)});\n`;
  const portData = `import { appendFileSync } from "node:fs";
import { workerData } from "node:worker_threads";
const post = workerData.port.postMessage.bind(workerData.port);\n`;

  const sources: Record<WorkerRole, string> = {
    log: `${portData}workerData.port.postMessage = (message) => {
  if (message.outcomes?.some((outcome) => outcome !== null))
    appendFileSync(${JSON.stringify(log)}, \`chunk \${message.chunk}\\n\`);
  post(message);
};
${importWorker}`,
    "late-post": `${portData}workerData.port.postMessage = (message) => {
  Bun.sleepSync(${LATE_POST_MS});
  post(message);
};
${importWorker}`,
    "post-after-finish": `${portData}const held = [];
workerData.port.postMessage = (message) => held.push(message);
${importWorker}const lanes = new Int32Array(workerData.lanes);
while (Atomics.load(lanes, workerData.id) !== 2) Bun.sleepSync(1);
Bun.sleepSync(${LATE_POST_MS});
for (const message of held) post(message);
setInterval(() => {}, 1000);
`,
    "setup-error-after-finish": `${portData}const held = [];
workerData.port.postMessage = (message) => held.push(message);
Object.defineProperty(workerData, "addon", {
  get() {
    throw new Error(${JSON.stringify(SETUP_FAILURE)});
  },
});
${importWorker}Bun.sleepSync(${HELD_SETUP_MS});
for (const message of held) post(message);
setInterval(() => {}, 1000);
`,
    load: `throw new Error(${JSON.stringify(LOAD_FAILURE)});\n`,
    "exit-at-load": `process.exit(${HARD_EXIT_CODE});\n`,
    "exit-after-serving": `import { workerData } from "node:worker_threads";
Object.defineProperty(workerData, "addon", {
  get() {
    Bun.sleepSync(${LATE_POST_MS});
    process.exit(${HARD_EXIT_CODE});
  },
});
${importWorker}`,
    "exit-on-post": `import { workerData } from "node:worker_threads";
workerData.port.postMessage = () => process.exit(${HARD_EXIT_CODE});
${importWorker}`,
    "throw-on-post": `import { workerData } from "node:worker_threads";
workerData.port.postMessage = () => {
  throw new Error(${JSON.stringify(POST_FAILURE)});
};
${importWorker}`,
    "throw-once": `import { workerData } from "node:worker_threads";
let thrown = false;
workerData.job = new Proxy(workerData.job, {
  get(target, key, receiver) {
    if (thrown) return Reflect.get(target, key, receiver);
    thrown = true;
    throw new Error(${JSON.stringify(CHUNK_FAILURE)});
  },
});
${importWorker}`,
  };

  return sources[role];
}

const neverClaims = new Set<WorkerRole>([
  "load",
  "exit-at-load",
  "exit-after-serving",
  "setup-error-after-finish",
]);

export function poolHarness(
  directory: string,
  worker: string,
  roles: WorkerRole[],
  blockUntilClaimed: boolean,
): PoolHarness {
  const log = join(directory, "worker-chunks");
  const modules = roles.map((role, index) => {
    const path = join(directory, `worker-${index}-${role}.ts`);
    writeFileSync(path, roleSource(role, worker, log));
    return path;
  });

  const preload = join(directory, "pool-preload.ts");
  writeFileSync(
    preload,
    `const threads = require("node:worker_threads");
const modules = ${JSON.stringify(modules)};
const claiming = ${JSON.stringify(roles.map((role) => !neverClaims.has(role)))};
let constructed = 0;
threads.Worker = class extends threads.Worker {
  constructor(specifier, options) {
    const index = constructed++;
    super(modules[index] ?? specifier, options);
    if (!${blockUntilClaimed} || claiming[index] !== true) return;

    const claims = new Int32Array(options.workerData.claims);
    const signal = new Int32Array(options.workerData.signal);
    const deadline = Date.now() + 10_000;
    while (!claims.includes(index + 1) && Date.now() < deadline)
      Atomics.wait(signal, 0, Atomics.load(signal, 0), 5);
  }
};\n`,
  );

  return {
    command: [process.execPath, "--preload", preload],
    workerChunks: () =>
      existsSync(log) ? readFileSync(log, "utf8").split("\n").filter(Boolean).length : 0,
  };
}
