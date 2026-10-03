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
import { availableParallelism } from "node:os";
import { join, relative } from "node:path";
import { deepRisk } from "../src/depth.ts";
import { CHUNK_SIZE, DEFAULT_WORKERS, THRESHOLD, workerCount } from "../src/pool.ts";
import { cli, run, scratch, scratchGitRepository } from "./support.ts";

const root = join(import.meta.dir, "..");
const fixtures = join(import.meta.dir, "fixtures");

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
  expect(workerCount(THRESHOLD, {})).toBe(
    Math.max(0, Math.min(DEFAULT_WORKERS, availableParallelism() - 1)),
  );

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
      const pooledResult = run(
        { cwd: pooled, env: { ...process.env, STANZA_WORKERS: "2" } },
        mode,
        ...flags,
        ".",
      );

      const serialResult = run(
        { cwd: serial, env: { ...process.env, STANZA_WORKERS: "0" } },
        mode,
        ...flags,
        ".",
      );

      expect(pooledResult).toEqual(serialResult);
      expect(snapshot(pooled)).toEqual(snapshot(serial));
    }
}, 60_000);

test("pooled deep inputs match serial check and fix across several chunks", () => {
  const directory = scratch("pool-deep");
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
    const pooledResult = run(
      { cwd: pooled, env: { ...process.env, STANZA_WORKERS: "2" } },
      mode,
      ".",
    );

    const serialResult = run(
      { cwd: serial, env: { ...process.env, STANZA_WORKERS: "0" } },
      mode,
      ".",
    );

    expect([0, 1]).toContain(pooledResult.code);
    expect(pooledResult).toEqual(serialResult);
    expect(snapshot(pooled)).toEqual(snapshot(serial));
  }
}, 60_000);

test("pooled fix keeps two hard links identical to serial", () => {
  const directory = scratch("pool-links");
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

  const pooledResult = run(
    { cwd: pooled, env: { ...process.env, STANZA_WORKERS: "2" } },
    "--fix",
    ".",
  );

  const serialResult = run(
    { cwd: serial, env: { ...process.env, STANZA_WORKERS: "0" } },
    "--fix",
    ".",
  );

  expect(pooledResult).toEqual(serialResult);
  expect(snapshot(pooled)).toEqual(snapshot(serial));
  expect(statSync(join(pooled, "0070.ts")).ino).toBe(statSync(join(pooled, "twin.ts")).ino);
  expect(statSync(join(pooled, "0070.ts")).nlink).toBe(2);
}, 60_000);

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

test("main finishes serially when every worker fails before claiming", () => {
  const directory = scratch("pool-load-failure");
  const preload = join(directory, "preload.ts");
  const worker = join(directory, "broken-worker.ts");
  const input = join(directory, "input.ts");

  writeFileSync(input, "if (a) {\n  b();\n}\n");
  writeFileSync(worker, 'throw new Error("worker load failed");\n');
  writeFileSync(
    preload,
    `const threads = require("node:worker_threads");
threads.Worker = class extends threads.Worker {
  constructor(specifier, options) {
    super(new URL(${JSON.stringify(worker)}, import.meta.url), options);
  }
};\n`,
  );

  const result = Bun.spawnSync([process.execPath, "--preload", preload, cli, "--check", input], {
    cwd: directory,
    env: { ...process.env, STANZA_WORKERS: "2" },
  });

  expect(result.exitCode).toBe(1);
  expect(result.stderr.toString()).toBe("");
  expect(result.stdout.toString()).toBe(
    run({ cwd: directory, env: { ...process.env, STANZA_WORKERS: "0" } }, "--check", input).stdout,
  );
});

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
  const worker = join(directory, "silent-worker.ts");
  const inputs = join(directory, "inputs");
  mkdirSync(inputs);

  for (let index = 0; index < CHUNK_SIZE * 3; index++)
    writeFileSync(join(inputs, `${String(index).padStart(4, "0")}.ts`), "if (a) {\n  b();\n}\n");

  writeFileSync(
    worker,
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
    super(new URL(${JSON.stringify(worker)}, import.meta.url), options);
    Atomics.wait(new Int32Array(options.workerData.signal), 0, 0, 5000);
  }
};\n`,
  );

  const result = Bun.spawnSync([process.execPath, "--preload", preload, cli, "--check", "."], {
    cwd: inputs,
    env: { ...process.env, STANZA_WORKERS: "1" },
    timeout: 30_000,
  });

  const serial = run({ cwd: inputs, env: { ...process.env, STANZA_WORKERS: "0" } }, "--check", ".");
  expect(result.exitCode).toBe(2);
  expect(result.stderr.toString()).toBe(
    "stanza: a worker thread stopped before reporting its files, so main formatted them\n",
  );

  expect(result.stdout.toString()).toBe(serial.stdout);
}, 60_000);
