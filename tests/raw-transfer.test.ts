import { expect, test } from "bun:test";
import { cpSync, existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { hostPlatform } from "../scripts/platform.ts";
import { scratch, spawnCli } from "./support.ts";

const root = join(import.meta.dir, "..");
const fixtures = join(import.meta.dir, "fixtures");
const entry = join(root, "src", "compile", `${hostPlatform()}.ts`);

function environment(mode: "0" | "1" | undefined): NodeJS.ProcessEnv {
  return { ...process.env, STANZA_RAW_TRANSFER: mode, FORCE_COLOR: undefined };
}

function snapshot(directory: string): Record<string, Buffer> {
  const files = readdirSync(directory, { recursive: true, withFileTypes: true }).filter((entry) =>
    entry.isFile(),
  );

  return Object.fromEntries(
    files.map((entry) => {
      const path = join(entry.parentPath, entry.name);
      return [path.slice(directory.length), readFileSync(path)];
    }),
  );
}

test("raw transfer deep inputs match JSON in check and fix", () => {
  const directory = scratch("raw-deep");
  const depth = 50_000;
  const expression = Array(depth).fill("x").join(" + ");
  const sources: [string, string][] = [
    ["plain.ts", `function f(x) { return ${expression}; }\n`],
    ["bound.ts", `function f(x) {\n  const n = 1;\n  return ${expression};\n}\n`],
    ["member.ts", `a.x = 1;\nif (a${".b".repeat(depth)}) a;\n`],
    ["else.ts", `${"if (x) {\n  x = 1;\n} else ".repeat(8_000)}{\n  x = 2;\n}\n`],
    ["guards.ts", `${"if (x) ".repeat(depth)}x = 1;\ny = 2;\n`],
    ["repeat.ts", `f();\n${"if (x) {\n".repeat(5_000)}f();\n${"}\n".repeat(5_000)}f();\n`],
  ];

  for (const [name, source] of sources)
    for (const command of ["--check", "--fix"]) {
      const path = join(directory, name);
      writeFileSync(path, source);

      const json = spawnCli({ cwd: directory, env: environment("0") }, command, name);
      const jsonBytes = readFileSync(path);
      writeFileSync(path, source);

      const raw = spawnCli({ cwd: directory, env: environment("1") }, command, name);
      expect([0, 1]).toContain(raw.code);
      expect(raw.stderr).toBe("");
      expect(raw).toEqual(json);
      expect(readFileSync(path)).toEqual(jsonBytes);
    }
}, 30_000);

test("raw transfer checks and fixes the fixture tree exactly like JSON", () => {
  const directory = scratch("raw-fixtures");
  const rawTree = join(directory, "raw");
  const jsonTree = join(directory, "json");
  cpSync(fixtures, rawTree, { recursive: true });
  cpSync(fixtures, jsonTree, { recursive: true });

  const rawCheck = spawnCli({ cwd: rawTree, env: environment("1") }, "--check", "--json", ".");
  const jsonCheck = spawnCli({ cwd: jsonTree, env: environment("0") }, "--check", "--json", ".");
  expect(rawCheck.stderr).toBe("");
  expect(rawCheck).toEqual(jsonCheck);

  const rawFix = spawnCli({ cwd: rawTree, env: environment("1") }, "--fix", ".");
  const jsonFix = spawnCli({ cwd: jsonTree, env: environment("0") }, "--fix", ".");
  expect(rawFix.stderr).toBe("");
  expect(rawFix).toEqual(jsonFix);
  expect(snapshot(rawTree)).toEqual(snapshot(jsonTree));
});

test("raw transfer preserves JS hashbang comments and TS parsing", () => {
  const directory = scratch("raw-hashbang");
  writeFileSync(join(directory, "script.js"), "#!/usr/bin/env node\nif (ready) {\n  run();\n}\n");
  writeFileSync(join(directory, "script.ts"), "const value: string = 'hello';\n");

  const raw = spawnCli({ cwd: directory, env: environment("1") }, "--check", "--json", ".");
  const json = spawnCli({ cwd: directory, env: environment("0") }, "--check", "--json", ".");
  expect(raw.stderr).toBe("");
  expect(raw).toEqual(json);
});

test("an unforced run switches to raw transfer partway and matches JSON", () => {
  const directory = scratch("raw-automatic");
  const body =
    "function f(a: number) {\n  const b = a;\n  if (b) {\n    return b;\n  }\n  return a;\n}\n";

  for (let index = 0; index < 80; index++)
    writeFileSync(join(directory, `file${index}.ts`), body.repeat(120));

  const automatic = spawnCli(
    { cwd: directory, env: environment(undefined) },
    "--check",
    "--json",
    ".",
  );

  const json = spawnCli({ cwd: directory, env: environment("0") }, "--check", "--json", ".");
  expect(automatic.stderr).toBe("");
  expect(automatic).toEqual(json);
}, 30_000);

test("a file over the raw transfer size cap is parsed through JSON", () => {
  const directory = scratch("raw-large");
  writeFileSync(join(directory, "large.ts"), "x;\nif (x) {\n  y();\n}\n".repeat(60_000));

  const raw = spawnCli({ cwd: directory, env: environment("1") }, "--check", "--json", ".");
  const json = spawnCli({ cwd: directory, env: environment("0") }, "--check", "--json", ".");
  expect(raw.stderr).toBe("");
  expect(raw).toEqual(json);
}, 30_000);

test.skipIf(!existsSync(entry))(
  "the compiled binary initializes raw transfer and matches source JSON",
  () => {
    const directory = scratch("raw-binary");
    const build = Bun.spawnSync([process.execPath, "scripts/build.ts", "--outdir", directory], {
      cwd: root,
      env: environment("1"),
    });

    expect(new TextDecoder().decode(build.stderr)).toBe("");
    expect(build.exitCode).toBe(0);

    const tree = join(directory, "fixtures");
    cpSync(fixtures, tree, { recursive: true });

    const compiled = Bun.spawnSync([join(directory, "stanza"), "--check", "."], {
      cwd: tree,
      env: environment("1"),
    });

    const source = spawnCli({ cwd: tree, env: environment("0") }, "--check", ".");
    expect(new TextDecoder().decode(compiled.stderr)).toBe("");
    expect(new TextDecoder().decode(compiled.stdout)).toBe(source.stdout);
    expect(compiled.exitCode).toBe(source.code);
  },
  { timeout: 60_000 },
);
