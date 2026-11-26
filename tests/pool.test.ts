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
import { cpuSpent } from "./budget.ts";
import { run, scratch } from "./support.ts";

const fixtures = join(import.meta.dir, "fixtures");
const THRESHOLD = 64;

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

function expectFormattedOrTooDeep(result: ReturnType<typeof run>): string[] {
  const findings = result.stdout.split("\n").filter(Boolean);
  const tooDeep = findings.filter((line) => line.split(": ")[1] === "parse");

  for (const line of findings) expect(["error", "write"]).not.toContain(line.split(": ")[1]);

  for (const line of tooDeep)
    expect(line).toEndWith(": parse: nests more than 10000 levels deep, left untouched");

  const [summary, ...after] = result.stderr.split("\n");
  expect(after).toEqual([""]);
  expect(summary).not.toStartWith("stanza:");
  expect(result.code).toBe(tooDeep.length > 0 ? 2 : findings.length > 0 ? 1 : 0);
  return tooDeep;
}

function pooledRun(cwd: string, ...args: string[]): ReturnType<typeof run> {
  return run({ cwd, env: { ...process.env, STANZA_WORKERS: "2" } }, ...args);
}

function serialRun(cwd: string, ...args: string[]): ReturnType<typeof run> {
  return run({ cwd, env: { ...process.env, STANZA_WORKERS: "0" } }, ...args);
}

interface DeepShape {
  name: string;
  deepest: number;
  jsx?: boolean;
  source: (depth: number) => string;
}

const deepShapes: DeepShape[] = [
  {
    name: "plain",
    deepest: 159_985,
    source: (depth) => `f = ${Array(depth).fill("x").join(" + ")};\n`,
  },
  { name: "member", deepest: 159_968, source: (depth) => `if (a${".b".repeat(depth)}) a;\n` },
  {
    name: "else",
    deepest: 9_999,
    source: (depth) => `${"if (x) {\n  x = 1;\n} else ".repeat(depth)}{\n}\n`,
  },
  {
    name: "elseLines",
    deepest: 9_999,
    source: (depth) => `${"if (x) {\n  x = 1;\n}\nelse ".repeat(depth)}{\n}\n`,
  },
  { name: "guards", deepest: 9_999, source: (depth) => `${"if (x) ".repeat(depth)}x = 1;\n` },
  {
    name: "guardLines",
    deepest: 9_999,
    source: (depth) => `${"while (x)\n".repeat(depth)}x = 1;\n`,
  },
  {
    name: "blocks",
    deepest: 9_998,
    source: (depth) => `${"if (x) {\n".repeat(depth)}f();\n${"}\n".repeat(depth)}`,
  },
  {
    name: "parens",
    deepest: 10_000,
    source: (depth) => `${"(".repeat(depth)}x${")".repeat(depth)};\n`,
  },
  {
    name: "arrays",
    deepest: 10_000,
    source: (depth) => `${"[1, ".repeat(depth)}x${"]".repeat(depth)};\n`,
  },
  {
    name: "regex",
    deepest: 10_000,
    source: (depth) => `const r = /^\\/*/, q = /"/; ${"(".repeat(depth)}x${")".repeat(depth)};\n`,
  },
  { name: "labels", deepest: 10_000, source: (depth) => `${"a: ".repeat(depth)}x;\n` },
  { name: "typeof", deepest: 159_984, source: (depth) => `y = ${"typeof ".repeat(depth)}x;\n` },
  { name: "await", deepest: 9_999, source: (depth) => `async () => ${"await ".repeat(depth)}x;\n` },
  { name: "not", deepest: 159_984, source: (depth) => `y = ${"!".repeat(depth)}x;\n` },
  { name: "new", deepest: 9_999, source: (depth) => `y = ${"new ".repeat(depth)}X;\n` },
  { name: "calls", deepest: 159_984, source: (depth) => `f${"()".repeat(depth)};\n` },
  { name: "index", deepest: 159_984, source: (depth) => `a${"[0]".repeat(depth)};\n` },
  { name: "assign", deepest: 10_000, source: (depth) => `${"a = ".repeat(depth)}1;\n` },
  { name: "ternary", deepest: 4_999, source: (depth) => `y = ${"a ? b : ".repeat(depth)}c;\n` },
  { name: "arrows", deepest: 9_999, source: (depth) => `f = ${"x => ".repeat(depth)}x;\n` },
  {
    name: "jsx",
    deepest: 9_997,
    jsx: true,
    source: (depth) =>
      `e = (\n${"<a>don't, stop; {x}\n".repeat(depth)}${"</a>\n".repeat(depth)});\n`,
  },
  {
    name: "templates",
    deepest: 4_999,
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

  const fixable = "if (probe) {\n  probe();\n}\n";
  const capped: string[] = [];
  const beyond = new Map<string, string>();
  for (const [block, shape] of deepShapes.entries())
    for (let offset = 0; offset < THRESHOLD; offset++) {
      const depth =
        offset === 0 ? shape.deepest : offset === THRESHOLD / 2 ? shape.deepest + 1 : undefined;

      const source =
        depth === undefined
          ? "function f(x) {\n  if (x) {\n    x();\n  }\n}\n".repeat(30)
          : shape.source(depth) + fixable;

      const name = depth === undefined ? "filler" : `${shape.name}-${depth}`;
      const index = String(block * THRESHOLD + offset).padStart(4, "0");
      const path = `${index}-${name}${shape.jsx ? ".tsx" : ".ts"}`;

      writeFileSync(join(pooled, path), source);
      writeFileSync(join(serial, path), source);
      if (depth === shape.deepest) capped.push(path);
      if (depth === shape.deepest + 1) beyond.set(path, source);
    }

  const written = snapshot(serial);
  for (const mode of ["--check", "--fix"]) {
    const pooledResult = pooledRun(pooled, mode, ".");
    const serialResult = serialRun(serial, mode, ".");

    if (mode === "--check") expect(snapshot(serial)).toEqual(written);

    expect(expectFormattedOrTooDeep(serialResult).map((line) => line.split(":")[0])).toEqual([
      ...beyond.keys(),
    ]);

    if (mode === "--check") {
      const braced = new Set(
        serialResult.stdout
          .split("\n")
          .filter((line) => line.split(": ")[1] === "braces")
          .map((line) => line.split(":")[0]),
      );

      expect(capped.filter((path) => !braced.has(path))).toEqual([]);
    }

    expect(pooledResult).toEqual(serialResult);
    expect(snapshot(pooled)).toEqual(snapshot(serial));
  }

  for (const path of capped)
    expect(readFileSync(join(serial, path), "utf8")).toEndWith("if (probe) probe();\n");

  for (const [path, source] of beyond)
    expect(readFileSync(join(serial, path), "utf8")).toBe(source);
}, 120_000);

test("pooled fix keeps two hard links identical to serial", () => {
  const directory = scratch("pool-links");
  const pooled = join(directory, "pooled");
  const serial = join(directory, "serial");
  for (const tree of [pooled, serial]) {
    mkdirSync(tree);

    for (let index = 0; index < THRESHOLD * 8; index++)
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

    for (let index = 0; index < THRESHOLD * 8; index++)
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
    expect(pooledResult.stderr).toBe(
      mode === "--check"
        ? "1 finding in 1 file, fixable with --fix\n"
        : "rewrote 1 file, no findings left\n",
    );

    expect(pooledResult).toEqual(serialRun(serial, mode, "."));
    expect(snapshot(pooled)).toEqual(snapshot(serial));
  }
}, 120_000);

test("the CLI and corpus deep inputs match serial for any STANZA_WORKERS", () => {
  const depth = 50_000;
  const expression = Array(depth).fill("x").join(" + ");
  const deepSources = Object.entries({
    plain: `function f(x) { return ${expression}; }\n`,
    bound: `function f(x) {\n  const n = 1;\n  return ${expression};\n}\n`,
    member: `a.x = 1;\nif (a${".b".repeat(depth)}) a;\n`,
    else: `${"if (x) {\n  x = 1;\n} else ".repeat(8_000)}{\n  x = 2;\n}\n`,
    guards: `${"if (x) ".repeat(depth)}x = 1;\ny = 2;\n`,
    repeat: `f();\n${"if (x) {\n".repeat(5_000)}f();\n${"}\n".repeat(5_000)}f();\n`,
    parens: `x = ${"(".repeat(3000)}y${")".repeat(3000)};\n`,
  });

  const directory = scratch("pool-any-count");
  const template = join(directory, "template");
  mkdirSync(template);
  const filler = "function f(x) {\n  if (x) {\n    x();\n  }\n}\n".repeat(30);
  const clean: string[] = [];
  for (let index = 0; index < THRESHOLD * 3; index++) {
    const [name, source] = (index % 27 === 0 ? deepSources[index / 27] : undefined) ?? [
      "filler",
      filler,
    ];

    const path = `${String(index).padStart(4, "0")}-${name}.ts`;
    writeFileSync(join(template, path), source);
    if (["plain", "bound", "member", "parens"].includes(name)) clean.push(path);
  }

  for (const [mode, stderr] of [
    ["--check", "checked 4 files, no findings\n"],
    ["--fix", "checked 4 files, nothing to fix\n"],
  ] as const)
    expect(serialRun(template, mode, ...clean)).toEqual({ code: 0, stdout: "", stderr });

  let serialMs = 0;
  for (const mode of ["--check", "--fix"]) {
    const serial = join(directory, `serial${mode}`);
    cpSync(template, serial, { recursive: true });
    const { value: serialResult, ms } = cpuSpent(() => serialRun(serial, mode, "."));
    serialMs += ms;
    expect(expectFormattedOrTooDeep(serialResult)).toEqual([
      expect.stringMatching(/^\d{4}-guards\.ts:/),
    ]);

    if (mode === "--check")
      expect(new Set(serialResult.stdout.match(/(?<=^\d{4}-)(?!filler\.)\w+(?=\.ts:)/gm))).toEqual(
        new Set(["else", "guards", "repeat"]),
      );

    for (const workers of ["1", "3", "64"]) {
      const pooled = join(directory, `pooled${mode}-${workers}`);
      cpSync(template, pooled, { recursive: true });
      const pooledResult = run(
        { cwd: pooled, env: { ...process.env, STANZA_WORKERS: workers } },
        mode,
        ".",
      );

      expect(pooledResult, `${mode} with ${workers} workers`).toEqual(serialResult);
      expect(snapshot(pooled)).toEqual(snapshot(serial));
    }
  }

  expect(serialMs).toBeLessThan(30_000);
}, 120_000);
