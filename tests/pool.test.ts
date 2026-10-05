import { expect, test } from "bun:test";
import {
  chmodSync,
  cpSync,
  linkSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { join, relative } from "node:path";
import { deepRisk } from "../src/depth.ts";
import { CHUNK_SIZE, DEFAULT_WORKERS, LANE, THRESHOLD, workerCount } from "../src/pool.ts";
import {
  HANG_LIMIT_MS,
  HARD_EXIT_CODE,
  LOAD_FAILURE,
  poolHarness,
  POST_FAILURE,
  SETUP_FAILURE,
  RUN_LIMIT_MS,
  type PoolHarness,
} from "./pool-harness.ts";
import { cli, run, scratch, scratchGitRepository } from "./support.ts";

const root = join(import.meta.dir, "..");
const fixtures = join(import.meta.dir, "fixtures");
const worker = join(root, "src", "worker.ts");
const braceSources = readdirSync(join(fixtures, "braces"))
  .filter((name) => name.endsWith(".before.ts"))
  .map((name) => readFileSync(join(fixtures, "braces", name), "utf8"));

function snapshot(directory: string): Record<string, { bytes: string; mode: number }> {
  return Object.fromEntries(
    readdirSync(directory, { recursive: true, withFileTypes: true })
      .filter((entry) => entry.isFile())
      .map((entry) => {
        const path = join(entry.parentPath, entry.name);
        return [
          relative(directory, path),
          { bytes: readFileSync(path).toString("base64"), mode: statSync(path).mode },
        ];
      }),
  );
}

function writeTree(tree: string, count: number): void {
  mkdirSync(tree, { recursive: true });

  for (let index = 0; index < count; index++)
    writeFileSync(
      join(tree, `${String(index).padStart(4, "0")}.ts`),
      braceSources[index % braceSources.length]!,
    );
}

function pooledRun(
  harness: PoolHarness,
  cwd: string,
  limit: number,
  ...args: string[]
): { code: number | null; stderr: string; stdout: string } {
  const result = Bun.spawnSync([...harness.command, cli, ...args], {
    cwd,
    env: { ...process.env, FORCE_COLOR: undefined, STANZA_WORKERS: "2" },
    timeout: limit,
  });

  return {
    code: result.exitCode,
    stderr: result.stderr.toString(),
    stdout: result.stdout.toString(),
  };
}

function serialRun(cwd: string, ...args: string[]): ReturnType<typeof run> {
  return run({ cwd, env: { ...process.env, STANZA_WORKERS: "0" } }, ...args);
}

function loggedHarness(name: string): { harness: PoolHarness; workedSince: () => boolean } {
  const harness = poolHarness(scratch(name), worker, ["log", "log"], true);
  let seen = 0;
  return {
    harness,
    workedSince() {
      const chunks = harness.workerChunks();
      const worked = chunks > seen;
      seen = chunks;
      return worked;
    },
  };
}

interface DeepShape {
  name: string;
  crash: number;
  jsx?: boolean;
  source: (depth: number) => string;
}

const deepShapes: DeepShape[] = [
  {
    name: "plain",
    crash: 29_184,
    source: (depth) => `f = ${Array(depth).fill("x").join(" + ")};\n`,
  },
  { name: "member", crash: 65_536, source: (depth) => `if (a${".b".repeat(depth)}) a;\n` },
  {
    name: "else",
    crash: 12_544,
    source: (depth) => `${"if (x) {\n  x = 1;\n} else ".repeat(depth)}{\n}\n`,
  },
  {
    name: "elseLines",
    crash: 12_544,
    source: (depth) => `${"if (x) {\n  x = 1;\n}\nelse ".repeat(depth)}{\n}\n`,
  },
  { name: "guards", crash: 12_544, source: (depth) => `${"if (x) ".repeat(depth)}x = 1;\n` },
  {
    name: "guardLines",
    crash: 12_544,
    source: (depth) => `${"while (x)\n".repeat(depth)}x = 1;\n`,
  },
  {
    name: "blocks",
    crash: 4_992,
    source: (depth) => `${"if (x) {\n".repeat(depth)}f();\n${"}\n".repeat(depth)}`,
  },
  {
    name: "parens",
    crash: 2_816,
    source: (depth) => `${"(".repeat(depth)}x${")".repeat(depth)};\n`,
  },
  {
    name: "arrays",
    crash: 2_816,
    source: (depth) => `${"[1, ".repeat(depth)}x${"]".repeat(depth)};\n`,
  },
  {
    name: "regex",
    crash: 2_816,
    source: (depth) => `const r = /^\\/*/, q = /"/; ${"(".repeat(depth)}x${")".repeat(depth)};\n`,
  },
  { name: "labels", crash: 10_112, source: (depth) => `${"a: ".repeat(depth)}x;\n` },
  { name: "typeof", crash: 29_184, source: (depth) => `y = ${"typeof ".repeat(depth)}x;\n` },
  { name: "await", crash: 20_224, source: (depth) => `async () => ${"await ".repeat(depth)}x;\n` },
  { name: "not", crash: 29_184, source: (depth) => `y = ${"!".repeat(depth)}x;\n` },
  { name: "new", crash: 11_904, source: (depth) => `y = ${"new ".repeat(depth)}X;\n` },
  { name: "calls", crash: 53_248, source: (depth) => `f${"()".repeat(depth)};\n` },
  { name: "index", crash: 65_536, source: (depth) => `a${"[0]".repeat(depth)};\n` },
  { name: "assign", crash: 8_448, source: (depth) => `${"a = ".repeat(depth)}1;\n` },
  { name: "ternary", crash: 6_272, source: (depth) => `y = ${"a ? b : ".repeat(depth)}c;\n` },
  { name: "arrows", crash: 4_864, source: (depth) => `f = ${"x => ".repeat(depth)}x;\n` },
  {
    name: "jsx",
    crash: 9_088,
    jsx: true,
    source: (depth) =>
      `e = (\n${"<a>don't, stop; {x}\n".repeat(depth)}${"</a>\n".repeat(depth)});\n`,
  },
  {
    name: "templates",
    crash: 2_496,
    source: (depth) => `y = ${"`${".repeat(depth)}x${"}`".repeat(depth)};\n`,
  },
];

test("worker count is bounded by default and the override forces an exact count", () => {
  expect(workerCount(THRESHOLD - 1, {})).toBe(0);
  expect(workerCount(THRESHOLD - 1, {}, 8, 64)).toBe(0);
  expect(workerCount(THRESHOLD, {}, undefined, 64)).toBe(DEFAULT_WORKERS);

  for (const [maximum, cores, count] of [
    [3, 1, 0],
    [3, 4, 3],
    [3, 64, 3],
    [8, 1, 0],
    [8, 4, 3],
    [8, 64, 8],
  ] as const)
    expect(workerCount(THRESHOLD, {}, maximum, cores), `${maximum} of ${cores}`).toBe(count);

  expect(workerCount(THRESHOLD, { STANZA_WORKERS: "2" }, 8, 64)).toBe(2);

  for (const count of [0, 1, 2, 64])
    for (const fileCount of [1, THRESHOLD * 2])
      expect(workerCount(fileCount, { STANZA_WORKERS: String(count) })).toBe(count);

  for (const value of ["", "-1", "1.5", "65", "Infinity", "NaN", " 2", "2x"])
    expect(workerCount(THRESHOLD * 2, { STANZA_WORKERS: value })).toBe(
      workerCount(THRESHOLD * 2, {}),
    );
});

test("deepRisk flags each measured worker crash shape at an eighth of its crash depth", () => {
  for (const shape of deepShapes)
    expect(
      deepRisk(shape.source(Math.floor(shape.crash / 8)), shape.jsx ?? false),
      shape.name,
    ).toBe(true);
});

test("deepRisk passes every source and fixture file and skips literal and comment text", () => {
  for (const directory of [join(root, "src"), fixtures])
    for (const entry of readdirSync(directory, { recursive: true, withFileTypes: true })) {
      if (!entry.isFile()) continue;
      const path = join(entry.parentPath, entry.name);
      expect(deepRisk(readFileSync(path, "utf8"), !path.endsWith(".ts")), path).toBe(false);
    }

  const deep = "(".repeat(400);
  for (const skipped of [
    `'${deep}'`,
    `"${deep}"`,
    `\`${deep}\``,
    `// ${deep}\n`,
    `/* ${deep} */`,
    `/${deep}/`,
  ])
    expect(deepRisk(`x = ${skipped};\n`, false), skipped.slice(0, 3)).toBe(false);

  expect(deepRisk(`x = [${"1, 2, ".repeat(10_000)}];\n`, false)).toBe(false);
  expect(deepRisk("f();\n".repeat(10_000), false)).toBe(false);
  expect(
    deepRisk(`e = a < b && (\n<ul>\n${"<li>x</li>\n<br />\n".repeat(1_000)}</ul>\n);\n`, true),
  ).toBe(false);
});

test("pooled check and fix match serial bytes, modes and output on many fixtures", () => {
  const directory = scratch("pool-fixtures");
  const { harness, workedSince } = loggedHarness("pool-fixtures-harness");
  const pooled = join(directory, "pooled");
  const serial = join(directory, "serial");
  for (let copy = 0; copy < 3; copy++) {
    cpSync(fixtures, join(pooled, String(copy)), { recursive: true });
    cpSync(fixtures, join(serial, String(copy)), { recursive: true });
  }

  chmodSync(join(pooled, "0", "braces", "example.before.ts"), 0o755);
  chmodSync(join(serial, "0", "braces", "example.before.ts"), 0o755);

  for (const mode of ["--check", "--fix"])
    for (const flags of [[], ["--braces", "--json"]]) {
      const pooledResult = pooledRun(harness, pooled, RUN_LIMIT_MS, mode, ...flags, ".");
      const serialResult = serialRun(serial, mode, ...flags, ".");
      expect(pooledResult).toEqual(serialResult);
      expect(snapshot(pooled)).toEqual(snapshot(serial));
      expect(workedSince()).toBe(true);
    }
}, 120_000);

test("pooled deep inputs match serial check and fix across several chunks", () => {
  const directory = scratch("pool-deep");
  const { harness, workedSince } = loggedHarness("pool-deep-harness");

  const pooled = join(directory, "pooled");
  const serial = join(directory, "serial");
  mkdirSync(pooled);
  mkdirSync(serial);

  for (let index = 0; index < CHUNK_SIZE * deepShapes.length; index++) {
    const shape = index % CHUNK_SIZE === 0 ? deepShapes[index / CHUNK_SIZE] : undefined;
    const source = shape
      ? shape.source(Math.ceil(shape.crash * 1.3))
      : "function f(x) {\n  if (x) {\n    x();\n  }\n}\n".repeat(30);

    const path = `${String(index).padStart(4, "0")}-${shape?.name ?? "filler"}${shape?.jsx ? ".tsx" : ".ts"}`;
    writeFileSync(join(pooled, path), source);
    writeFileSync(join(serial, path), source);
  }

  for (const mode of ["--check", "--fix"]) {
    const pooledResult = pooledRun(harness, pooled, RUN_LIMIT_MS, mode, ".");
    const serialResult = serialRun(serial, mode, ".");

    expect([0, 1]).toContain(serialResult.code);
    expect(pooledResult).toEqual(serialResult);
    expect(snapshot(pooled)).toEqual(snapshot(serial));
    expect(workedSince()).toBe(true);
  }
}, 120_000);

test("pooled fix keeps two hard links identical to serial", () => {
  const directory = scratch("pool-links");
  const { harness, workedSince } = loggedHarness("pool-links-harness");
  const pooled = join(directory, "pooled");
  const serial = join(directory, "serial");
  for (const tree of [pooled, serial]) {
    mkdirSync(tree);

    for (let index = 0; index < CHUNK_SIZE * 8; index++)
      writeFileSync(
        join(tree, `${String(index).padStart(4, "0")}.ts`),
        "function f(x) {\n  if (x) {\n    x();\n  }\n}\n".repeat(30),
      );

    const first = join(tree, "0070.ts");
    writeFileSync(first, "if (a) {\n  b();\n}\nc();\n");
    chmodSync(first, 0o751);
    linkSync(first, join(tree, "twin.ts"));
  }

  const pooledResult = pooledRun(harness, pooled, RUN_LIMIT_MS, "--fix", ".");
  const serialResult = serialRun(serial, "--fix", ".");

  expect(pooledResult).toEqual(serialResult);
  expect(snapshot(pooled)).toEqual(snapshot(serial));
  expect(statSync(join(pooled, "0070.ts")).ino).toBe(statSync(join(pooled, "twin.ts")).ino);
  expect(statSync(join(pooled, "0070.ts")).nlink).toBe(2);
  expect(workedSince()).toBe(true);
}, 120_000);

test("a preload counts Worker construction for pooled runs only and sees none terminated", () => {
  const directory = scratchGitRepository();
  const preload = join(directory, "count.ts");
  const counts = join(directory, "counts");
  const input = join(directory, "input.ts");
  writeFileSync(input, "if (a) {\n  b();\n}\n");
  writeFileSync(
    preload,
    `const threads = require("node:worker_threads") as typeof import("node:worker_threads");
import { appendFileSync } from "node:fs";
threads.Worker = class extends threads.Worker {
    constructor(...args: ConstructorParameters<typeof threads.Worker>) {
      super(...args);
      appendFileSync(${JSON.stringify(counts)}, "constructed\\n");
    }
    terminate() {
      appendFileSync(${JSON.stringify(counts)}, "terminated\\n");
      return super.terminate();
    }
};\n`,
  );

  const command = [process.execPath, "--preload", preload, cli];
  const env = { ...process.env, STANZA_WORKERS: "0" };
  const small = Bun.spawnSync([...command, "--check", input], {
    cwd: directory,
    env: { ...env, STANZA_WORKERS: undefined },
  });

  expect(small.exitCode).toBe(1);
  expect(small.stderr.toString()).toBe("");
  expect(readdirSync(directory)).not.toContain("counts");

  const hook = Bun.spawnSync([...command, "hook"], {
    cwd: directory,
    env: { ...env, STANZA_WORKERS: "2" },
    stdin: new TextEncoder().encode(
      JSON.stringify({
        hook_event_name: "PreToolUse",
        tool_name: "Write",
        tool_input: { file_path: input, content: readFileSync(input, "utf8") },
      }),
    ),
  });

  expect(hook.exitCode).toBe(0);
  expect(hook.stderr.toString()).toBe("");
  expect(hook.stdout.toString()).toContain("updatedInput");
  expect(readdirSync(directory)).not.toContain("counts");

  const pooled = Bun.spawnSync([...command, "--check", input], {
    cwd: directory,
    env: { ...env, STANZA_WORKERS: "2" },
  });

  expect(pooled.exitCode).toBe(1);
  expect(pooled.stderr.toString()).toBe("");
  expect(readFileSync(counts, "utf8")).toBe("constructed\nconstructed\n");
}, 60_000);

test("a worker that fails to load is reported and the run matches serial", () => {
  const directory = scratch("pool-load");
  const harness = poolHarness(scratch("pool-load-harness"), worker, ["load", "log"], true);

  const pooled = join(directory, "pooled");
  const serial = join(directory, "serial");
  writeTree(pooled, 320);
  writeTree(serial, 320);

  for (const mode of ["--check", "--fix"]) {
    const before = harness.workerChunks();
    const pooledResult = pooledRun(harness, pooled, RUN_LIMIT_MS, mode, ".");
    const serialResult = serialRun(serial, mode, ".");
    expect(pooledResult).toEqual({
      ...serialResult,
      code: 2,
      stderr: `stanza: a worker thread failed to start, so main did its share: ${LOAD_FAILURE}\n`,
    });

    expect(snapshot(pooled)).toEqual(snapshot(serial));
    expect(harness.workerChunks()).toBeGreaterThan(before);
  }
}, 60_000);

test("a worker that hard exits after claiming a chunk is reported and the run matches serial", () => {
  const directory = scratch("pool-hard-exit");
  const harness = poolHarness(
    scratch("pool-hard-exit-harness"),
    worker,
    ["exit-on-post", "log"],
    true,
  );

  const pooled = join(directory, "pooled");
  const serial = join(directory, "serial");
  writeTree(pooled, 320);
  writeTree(serial, 320);

  for (const mode of ["--check", "--fix"]) {
    const pooledResult = pooledRun(harness, pooled, HANG_LIMIT_MS, mode, ".");
    const serialResult = serialRun(serial, mode, ".");
    expect(pooledResult).toEqual({
      ...serialResult,
      code: 2,
      stderr: `stanza: a worker thread exited with code ${HARD_EXIT_CODE} before reporting ${CHUNK_SIZE} files, so main took them over\n`,
    });

    expect(snapshot(pooled)).toEqual(snapshot(serial));
    expect(readdirSync(pooled).some((name) => name.startsWith(".stanza-"))).toBe(false);
  }
}, 60_000);

test("main waits for results a live worker posted before it finished but main has not drained", () => {
  const directory = scratch("pool-undrained");
  const harness = poolHarness(
    scratch("pool-undrained-harness"),
    worker,
    ["post-after-finish", "log"],
    true,
  );

  writeTree(directory, 320);

  expect(pooledRun(harness, directory, HANG_LIMIT_MS, "--check", ".")).toEqual(
    serialRun(directory, "--check", "."),
  );
}, 60_000);

test("main waits for a setup error a finished worker posted after main did every chunk", () => {
  const directory = scratch("pool-held-setup");
  const harness = poolHarness(
    scratch("pool-held-setup-harness"),
    worker,
    ["setup-error-after-finish"],
    true,
  );

  writeTree(directory, 1);

  expect(pooledRun(harness, directory, HANG_LIMIT_MS, "--check", ".")).toEqual({
    ...serialRun(directory, "--check", "."),
    code: 2,
    stderr: `stanza: ${SETUP_FAILURE}\n`,
  });
}, 60_000);

test("main stays alive while it waits for workers that report after it ran out of chunks", () => {
  const directory = scratch("pool-late");
  const harness = poolHarness(
    scratch("pool-late-harness"),
    worker,
    ["late-post", "late-post"],
    true,
  );

  writeTree(directory, 320);

  expect(pooledRun(harness, directory, RUN_LIMIT_MS, "--check", ".")).toEqual(
    serialRun(directory, "--check", "."),
  );
}, 60_000);

test("every worker failing to load is reported and main formats every file", () => {
  const directory = scratch("pool-load-failure");
  const harness = poolHarness(scratch("pool-load-failure-harness"), worker, ["load", "load"], true);
  const input = join(directory, "input.ts");
  writeFileSync(input, "if (a) {\n  b();\n}\n");

  const loss = `stanza: a worker thread failed to start, so main did its share: ${LOAD_FAILURE}\n`;
  expect(pooledRun(harness, directory, RUN_LIMIT_MS, "--check", input)).toEqual({
    ...serialRun(directory, "--check", input),
    code: 2,
    stderr: loss.repeat(2),
  });
});

for (const [role, fault, loss] of [
  [
    "exit-at-load",
    "exits while loading",
    `a worker thread failed to start, so main did its share: exited with code ${HARD_EXIT_CODE}`,
  ],
  [
    "exit-after-serving",
    "exits before claiming files",
    `a worker thread exited with code ${HARD_EXIT_CODE}, so main did its share`,
  ],
] as const)
  test(`a worker that ${fault} is reported and the run matches serial`, () => {
    const directory = scratch("pool-early-exit");
    const harness = poolHarness(scratch("pool-early-exit-harness"), worker, [role, "log"], true);
    writeTree(directory, 1);

    expect(pooledRun(harness, directory, HANG_LIMIT_MS, "--check", ".")).toEqual({
      ...serialRun(directory, "--check", "."),
      code: 2,
      stderr: `stanza: ${loss}\n`,
    });
  }, 60_000);

test("a worker that throws uncaught after claiming a chunk is reported with its error", () => {
  const directory = scratch("pool-uncaught");
  const harness = poolHarness(
    scratch("pool-uncaught-harness"),
    worker,
    ["throw-on-post", "log"],
    false,
  );

  writeTree(directory, 320);

  expect(pooledRun(harness, directory, HANG_LIMIT_MS, "--check", ".")).toEqual({
    ...serialRun(directory, "--check", "."),
    code: 2,
    stderr: `stanza: a worker thread failed before reporting ${CHUNK_SIZE} files, so main took them over: ${POST_FAILURE}\n`,
  });
}, 60_000);

test("a chunk error returns 2 after other claimed chunks finish their writes", () => {
  const directory = scratch("pool-chunk-error");
  const preload = join(directory, "preload.ts");
  const worker = join(directory, "error-worker.ts");
  const inputs = join(directory, "inputs");
  mkdirSync(inputs);

  for (let index = 0; index < CHUNK_SIZE * 3; index++)
    writeFileSync(join(inputs, `${String(index).padStart(4, "0")}.ts`), "if (a) {\n  b();\n}\n");

  writeFileSync(
    worker,
    `import { workerData as data } from "node:worker_threads";
const counter = new Int32Array(data.counter);
const signal = new Int32Array(data.signal);
Atomics.store(new Int32Array(data.lanes), data.id, ${LANE.serving});
const chunk = Atomics.add(counter, 0, 1);
data.port.postMessage({ chunk, error: "injected chunk failure" });
Atomics.add(signal, 0, 1);
Atomics.notify(signal, 0);\n`,
  );

  writeFileSync(
    preload,
    `const threads = require("node:worker_threads");
threads.Worker = class extends threads.Worker {
  constructor(specifier, options) {
    super(new URL(${JSON.stringify(worker)}, import.meta.url), options);
    Atomics.wait(new Int32Array(options.workerData.signal), 0, 0, 5000);
  }
};\n`,
  );

  const result = Bun.spawnSync([process.execPath, "--preload", preload, cli, "--fix", inputs], {
    cwd: inputs,
    env: { ...process.env, STANZA_WORKERS: "1" },
  });

  expect(result.exitCode).toBe(2);
  expect(result.stderr.toString()).toBe("stanza: injected chunk failure\n");
  expect(readFileSync(join(inputs, "0000.ts"), "utf8")).toBe("if (a)\n  b();\n");
  expect(readFileSync(join(inputs, "0191.ts"), "utf8")).toBe("if (a)\n  b();\n");
  expect(readdirSync(inputs).some((name) => name.startsWith(".stanza-"))).toBe(false);
});

test("main formats a chunk whose worker stopped without reporting it", () => {
  const directory = scratch("pool-stopped-worker");
  const preload = join(directory, "preload.ts");
  const silent = join(directory, "silent-worker.ts");
  const inputs = join(directory, "inputs");
  mkdirSync(inputs);

  for (let index = 0; index < CHUNK_SIZE * 3; index++)
    writeFileSync(join(inputs, `${String(index).padStart(4, "0")}.ts`), "if (a) {\n  b();\n}\n");

  writeFileSync(
    silent,
    `import { workerData } from "node:worker_threads";
workerData.port.postMessage = () => {
  throw new Error("clone failed");
};
await import(${JSON.stringify(join(root, "src", "worker.ts"))});\n`,
  );

  writeFileSync(
    preload,
    `const threads = require("node:worker_threads");
threads.Worker = class extends threads.Worker {
  constructor(specifier, options) {
    super(new URL(${JSON.stringify(silent)}, import.meta.url), options);
    const lanes = new Int32Array(options.workerData.lanes);
    const signal = new Int32Array(options.workerData.signal);
    const deadline = Date.now() + 10_000;
    while (Atomics.load(lanes, 0) !== ${LANE.finished} && Date.now() < deadline)
      Atomics.wait(signal, 0, Atomics.load(signal, 0), 5);
  }
};\n`,
  );

  const result = Bun.spawnSync([process.execPath, "--preload", preload, cli, "--check", "."], {
    cwd: inputs,
    env: { ...process.env, STANZA_WORKERS: "1" },
    timeout: 30_000,
  });

  const serial = serialRun(inputs, "--check", ".");
  expect(result.exitCode).toBe(2);
  expect(result.stderr.toString()).toBe(
    `stanza: a worker thread failed before reporting ${CHUNK_SIZE} files, so main took them over: clone failed\n`,
  );

  expect(result.stdout.toString()).toBe(serial.stdout);
}, 60_000);

test("main recovers a chunk whose worker died before recording its claim", () => {
  const directory = scratch("pool-unrecorded-claim");
  const preload = join(directory, "preload.ts");
  const dying = join(directory, "dying-worker.ts");
  const inputs = join(directory, "inputs");
  writeTree(inputs, CHUNK_SIZE * 3);
  writeFileSync(
    dying,
    `import { workerData } from "node:worker_threads";
Atomics.store(new Int32Array(workerData.lanes), workerData.id, ${LANE.serving});
Atomics.add(new Int32Array(workerData.counter), 0, 1);
process.exit(${HARD_EXIT_CODE});\n`,
  );

  writeFileSync(
    preload,
    `const threads = require("node:worker_threads");
threads.Worker = class extends threads.Worker {
  constructor(specifier, options) {
    super(${JSON.stringify(dying)}, options);
    const counter = new Int32Array(options.workerData.counter);
    const signal = new Int32Array(options.workerData.signal);
    const deadline = Date.now() + 10_000;
    while (Atomics.load(counter, 0) === 0 && Date.now() < deadline)
      Atomics.wait(signal, 0, Atomics.load(signal, 0), 5);
  }
};\n`,
  );

  const result = Bun.spawnSync([process.execPath, "--preload", preload, cli, "--check", "."], {
    cwd: inputs,
    env: { ...process.env, STANZA_WORKERS: "1" },
    timeout: HANG_LIMIT_MS,
  });

  expect(result.exitCode).toBe(2);
  expect(result.stderr.toString()).toBe(
    `stanza: a worker thread exited with code ${HARD_EXIT_CODE}, so main did its share\nstanza: a worker thread stopped before reporting ${CHUNK_SIZE} files, so main took them over\n`,
  );

  expect(result.stdout.toString()).toBe(serialRun(inputs, "--check", ".").stdout);
}, 60_000);

test("a run that starts no worker loads neither worker_threads nor the depth check", () => {
  const directory = scratch("pool-modules");
  const preload = join(directory, "modules.ts");
  const modules = join(directory, "modules");
  const tree = join(directory, "tree");
  writeTree(tree, 300);
  writeFileSync(
    preload,
    `import { writeFileSync } from "node:fs";
const Module = require("node:module");
const required = [];
const original = Module.prototype.require;
Module.prototype.require = function (id) {
  required.push(id);
  return original.call(this, id);
};
process.on("exit", () =>
  writeFileSync(${JSON.stringify(modules)}, [...Object.keys(require.cache), ...required].join("\\n")),
);\n`,
  );

  function loaded(workers: string | undefined, target: string): string[] {
    const result = Bun.spawnSync([process.execPath, "--preload", preload, cli, "--check", target], {
      cwd: directory,
      env: { ...process.env, STANZA_WORKERS: workers },
      timeout: 30_000,
    });

    expect(result.stderr.toString()).toBe("");
    expect(result.exitCode).toBe(1);
    return readFileSync(modules, "utf8").split("\n");
  }

  for (const [workers, target] of [
    [undefined, join(tree, "0000.ts")],
    ["0", tree],
  ] as const) {
    const ids = loaded(workers, target);
    expect(ids).toContain(cli);
    expect(ids).not.toContain("node:worker_threads");
    expect(ids).not.toContain(join(root, "src", "depth.ts"));
  }

  expect(loaded("2", tree)).toContain("node:worker_threads");
}, 60_000);
