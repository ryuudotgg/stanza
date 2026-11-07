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
import { run, scratch, spawnCli } from "./support.ts";

const fixtures = join(import.meta.dir, "fixtures");
const CHUNK_SIZE = 64;

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

function pooledRun(cwd: string, ...args: string[]): ReturnType<typeof run> {
  return spawnCli({ cwd, env: { ...process.env, STANZA_WORKERS: "2" } }, ...args);
}

function serialRun(cwd: string, ...args: string[]): ReturnType<typeof run> {
  return run({ cwd, env: { ...process.env, STANZA_WORKERS: "0" } }, ...args);
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
      const pooledResult = pooledRun(pooled, mode, ...flags, ".");
      const serialResult = serialRun(serial, mode, ...flags, ".");
      expect(pooledResult).toEqual(serialResult);
      expect(snapshot(pooled)).toEqual(snapshot(serial));
    }
}, 120_000);

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
    const pooledResult = pooledRun(pooled, mode, ".");
    const serialResult = serialRun(serial, mode, ".");

    expect([0, 1]).toContain(serialResult.code);
    expect(pooledResult).toEqual(serialResult);
    expect(snapshot(pooled)).toEqual(snapshot(serial));
  }
}, 120_000);

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

  const pooledResult = pooledRun(pooled, "--fix", ".");
  const serialResult = serialRun(serial, "--fix", ".");

  expect(pooledResult).toEqual(serialResult);
  expect(snapshot(pooled)).toEqual(snapshot(serial));
  expect(statSync(join(pooled, "0070.ts")).ino).toBe(statSync(join(pooled, "twin.ts")).ino);
  expect(statSync(join(pooled, "0070.ts")).nlink).toBe(2);
}, 120_000);

test("pooled header generated selections print the generated reason line", () => {
  const directory = scratch("pool-generated");
  const generated = `// @generated\n${"function f(x) {\n  if (x) {\n    x();\n  }\n}\n".repeat(30)}`;
  const pooled = join(directory, "pooled");
  const serial = join(directory, "serial");
  for (const tree of [pooled, serial]) {
    mkdirSync(tree);

    for (let index = 0; index < CHUNK_SIZE * 8; index++)
      writeFileSync(join(tree, `${String(index).padStart(4, "0")}.ts`), generated);
  }

  for (const mode of ["--check", "--fix"])
    expect(pooledRun(pooled, mode, ".")).toEqual({
      code: 0,
      stderr: "stanza: nothing to format in ., generated files are skipped\n",
      stdout: "",
    });

  for (const tree of [pooled, serial])
    writeFileSync(join(tree, "plain.ts"), "if (a) {\n  b();\n}\n");

  for (const mode of ["--check", "--fix"]) {
    const pooledResult = pooledRun(pooled, mode, ".");
    expect(pooledResult.stderr).toBe("");
    expect(pooledResult).toEqual(serialRun(serial, mode, "."));
    expect(snapshot(pooled)).toEqual(snapshot(serial));
  }
}, 120_000);

test("the CLI and corpus deep inputs match serial for any STANZA_WORKERS", () => {
  const depth = 50_000;
  const expression = Array(depth).fill("x").join(" + ");
  const deepSources = [
    `function f(x) { return ${expression}; }\n`,
    `function f(x) {\n  const n = 1;\n  return ${expression};\n}\n`,
    `a.x = 1;\nif (a${".b".repeat(depth)}) a;\n`,
    `${"if (x) {\n  x = 1;\n} else ".repeat(8_000)}{\n  x = 2;\n}\n`,
    `${"if (x) ".repeat(depth)}x = 1;\ny = 2;\n`,
    `f();\n${"if (x) {\n".repeat(5_000)}f();\n${"}\n".repeat(5_000)}f();\n`,
    `x = ${"(".repeat(3000)}y${")".repeat(3000)};\n`,
  ];

  const directory = scratch("pool-any-count");
  const template = join(directory, "template");
  mkdirSync(template);
  const filler = "function f(x) {\n  if (x) {\n    x();\n  }\n}\n".repeat(30);
  for (let index = 0; index < CHUNK_SIZE * 3; index++)
    writeFileSync(
      join(template, `${String(index).padStart(4, "0")}.ts`),
      (index % 27 === 0 ? deepSources[index / 27] : undefined) ?? filler,
    );

  for (const mode of ["--check", "--fix"]) {
    const serial = join(directory, `serial${mode}`);
    cpSync(template, serial, { recursive: true });
    const serialResult = serialRun(serial, mode, ".");
    expect([0, 1]).toContain(serialResult.code);
    expect(serialResult.stderr).toBe("");

    for (const workers of ["1", "3", "64"]) {
      const pooled = join(directory, `pooled${mode}-${workers}`);
      cpSync(template, pooled, { recursive: true });
      const pooledResult = spawnCli(
        { cwd: pooled, env: { ...process.env, STANZA_WORKERS: workers } },
        mode,
        ".",
      );

      expect(pooledResult, `${mode} with ${workers} workers`).toEqual(serialResult);
      expect(snapshot(pooled)).toEqual(snapshot(serial));
    }
  }
}, 120_000);
