import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import {
  cpSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { decode, formatText, type StepResult } from "../src/step.ts";
import type { Mode } from "../src/engine/types.ts";
import { scratch } from "./support.ts";

const repo = join(import.meta.dir, "..");
const fixtures = join(import.meta.dir, "fixtures");

const pins: Record<string, string> = JSON.parse(
  readFileSync(join(repo, "package.json"), "utf8"),
).devDependencies;

interface Formatter {
  name: string;
  pkg: string;
  command: string[];
  rejects: string[];
}

const formatters: Formatter[] = [
  { name: "oxfmt", pkg: "oxfmt", command: ["oxfmt"], rejects: [] },
  {
    name: "biome",
    pkg: "@biomejs/biome",
    command: ["biome", "format", "--write"],
    // Biome parses every .js file as a strict mode module, where `with` is a syntax error.
    rejects: ["braces/dangling-with.after.js"],
  },
  {
    name: "prettier",
    pkg: "prettier",
    command: ["prettier", "--write", "--log-level=warn"],
    rejects: [],
  },
];

interface Unfixable {
  file: string;
  rule: string;
  line: string;
}

const unfixable: Unfixable[] = [
  {
    file: "block-spacing/blocks.after.ts",
    rule: "block-spacing",
    line: "for (const entry of items) {",
  },
  {
    file: "block-spacing/else-arms-broken.after.ts",
    rule: "block-spacing",
    line: 'if (ready && method !== "HEAD") response.write(body);',
  },
  {
    file: "block-spacing/else-if-empty-block.after.ts",
    rule: "block-spacing",
    line: 'if (ready && method !== "HEAD") response.write(body);',
  },
  {
    file: "block-spacing/example.after.ts",
    rule: "block-spacing",
    line: "for (const field of form.fields) {",
  },
  { file: "reference-precision/precision.after.ts", rule: "block-spacing", line: "try {" },
  {
    file: "wall/example.after.ts",
    rule: "wall",
    line: "const config = loadConfig(options.configPath);",
  },
  { file: "wall/forced-and-clearable.after.ts", rule: "wall", line: "console.log(0);" },
  { file: "wall/wall.after.ts", rule: "wall", line: "const a = 1;" },
];

interface Tool {
  name: string;
  run(dir: string, files: string[]): Promise<void>;
}

interface Move {
  tool: string;
  file: string;
}

function pin(dir: string, formatter: Formatter): void {
  writeFileSync(
    join(dir, "package.json"),
    JSON.stringify({ devDependencies: { [formatter.pkg]: pins[formatter.pkg] } }),
  );
}

const formatted = new Map<string, string>();

function cacheKey(formatter: Formatter, path: string, text: string): string {
  return JSON.stringify([formatter.name, path, text]);
}

async function runFormatter(formatter: Formatter, sources: Map<string, string>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), `stanza-${formatter.name}-`));
  try {
    for (const [path, text] of sources) {
      mkdirSync(dirname(join(dir, path)), { recursive: true });
      writeFileSync(join(dir, path), text);
    }

    const [bin, ...flags] = formatter.command;
    const child = Bun.spawn(
      [join(repo, "node_modules", ".bin", bin!), ...flags, ...sources.keys()],
      {
        cwd: dir,
        stdout: "ignore",
        stderr: "pipe",
      },
    );

    const stderr = await new Response(child.stderr).text();
    if ((await child.exited) !== 0)
      throw new Error(`${formatter.name} exited ${child.exitCode}: ${stderr}`);

    for (const [path, text] of sources)
      formatted.set(cacheKey(formatter, path, text), readFileSync(join(dir, path), "utf8"));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

async function format(formatter: Formatter, paths: string[], texts: string[]): Promise<string[]> {
  const keys = paths.map((path, index) => cacheKey(formatter, path, texts[index]!));
  const missed = new Map(
    paths.flatMap((path, index) => (formatted.has(keys[index]!) ? [] : [[path, texts[index]!]])),
  );

  if (missed.size > 0) await runFormatter(formatter, missed);
  return keys.map((key) => formatted.get(key)!);
}

function formatterTool(formatter: Formatter, fixture: string): Tool {
  return {
    name: formatter.name,
    async run(dir, files) {
      const outputs = await format(
        formatter,
        files.map((file) => `${fixture}/${file}`),
        snapshot(dir, files),
      );

      for (const [index, file] of files.entries()) writeFileSync(join(dir, file), outputs[index]!);
    },
  };
}

function stanzaFile(dir: string, file: string, mode: Mode): StepResult {
  const path = join(dir, file);
  const result = formatText(path, decode(readFileSync(path)), { mode });
  if (result.parseError) throw new Error(`stanza failed to parse ${file}`);
  return result;
}

const stanza: Tool = {
  name: "stanza --fix",
  async run(dir, files) {
    for (const file of files) {
      const { fixed } = stanzaFile(dir, file, "fix");
      if (fixed !== undefined) writeFileSync(join(dir, file), fixed, "utf8");
    }
  },
};

function snapshot(dir: string, files: string[]): string[] {
  return files.map((file) => readFileSync(join(dir, file), "utf8"));
}

async function runRound(dir: string, files: string[], round: Tool[]): Promise<Move[]> {
  const moves: Move[] = [];
  for (const tool of round) {
    const before = snapshot(dir, files);
    await tool.run(dir, files);

    const after = snapshot(dir, files);
    for (const [index, file] of files.entries())
      if (after[index] !== before[index]) moves.push({ tool: tool.name, file });
  }

  return moves;
}

function unexpected(label: string, dir: string, fixture: string, file: string): string[] {
  const path = `${fixture}/${file}`;
  const lines = readFileSync(join(dir, file), "utf8").split(/\r?\n/);
  const allowed = unfixable.filter((entry) => entry.file === path);

  return stanzaFile(dir, file, "check")
    .findings.filter((finding) => {
      const line = lines[finding.line - 1]!.trim();
      const index = allowed.findIndex(
        (entry) => entry.rule === finding.rule && entry.line === line,
      );

      if (index === -1) return true;

      allowed.splice(index, 1);
      return false;
    })
    .map((finding) => `${label} ${path}:${finding.line}:${finding.col} ${finding.rule}`);
}

const fixtureFiles = readdirSync(fixtures)
  .sort()
  .map((name) => ({
    name,
    files: readdirSync(join(fixtures, name))
      .filter((file) => /\.after\.[jt]sx?$/.test(file))
      .sort(),
  }));

const prepared: string[] = [];

beforeAll(async () => {
  await Promise.all(
    formatters.map(async (formatter) => {
      const paths = fixtureFiles
        .flatMap(({ name, files }) => files.map((file) => `${name}/${file}`))
        .filter((path) => !formatter.rejects.includes(path));

      const dir = mkdtempSync(join(tmpdir(), `stanza-${formatter.name}-`));
      prepared.push(dir);
      cpSync(fixtures, dir, { recursive: true });
      pin(dir, formatter);

      for (let round = 0; round < 3; round++) {
        const outputs = await format(formatter, paths, snapshot(dir, paths));
        for (const [index, path] of paths.entries())
          writeFileSync(join(dir, path), outputs[index]!);

        await stanza.run(dir, paths);
      }
    }),
  );
}, 60_000);

afterAll(() => {
  for (const dir of prepared) rmSync(dir, { recursive: true, force: true });
});

for (const formatter of formatters)
  for (const { name, files } of fixtureFiles) {
    if (files.length === 0) continue;

    describe(`${name}: stanza and ${formatter.name} settle in one round`, () => {
      for (const file of files.filter((file) => !formatter.rejects.includes(`${name}/${file}`)))
        test(file, async () => {
          const dir = scratch(formatter.name);
          cpSync(join(fixtures, name), dir, { recursive: true });
          pin(dir, formatter);

          const round = [formatterTool(formatter, name), stanza];
          const moves: Move[][] = [];
          for (let index = 1; index <= 2; index++) {
            moves.push(await runRound(dir, [file], round));
            expect(unexpected(`${formatter.name} round ${index}`, dir, name, file)).toEqual([]);
          }

          expect(moves.at(-1)).toEqual([]);
        });
    });
  }

test("transforms that undo each other never settle", async () => {
  const dir = scratch("oxfmt");
  writeFileSync(join(dir, "a.ts"), "export const a = 1;\n");

  const marker = "// added\n";
  const add: Tool = {
    name: "add",
    async run(at, files) {
      for (const file of files)
        writeFileSync(join(at, file), marker + readFileSync(join(at, file), "utf8"));
    },
  };

  const strip: Tool = {
    name: "strip",
    async run(at, files) {
      for (const file of files)
        writeFileSync(join(at, file), readFileSync(join(at, file), "utf8").replace(marker, ""));
    },
  };

  await runRound(dir, ["a.ts"], [add, strip]);
  expect(await runRound(dir, ["a.ts"], [add, strip])).toEqual([
    { tool: "add", file: "a.ts" },
    { tool: "strip", file: "a.ts" },
  ]);
});
