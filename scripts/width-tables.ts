import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { util } from "prettier";

const root = join(import.meta.dir, "..");
const bin = join(root, "node_modules", ".bin");
const target = join(root, "crates", "stanza", "src", "engine", "widths.rs");

const PRINT_WIDTH = 40;
const NARROW = 3;
const WIDE = 16;

const CHUNK = 0x10000;
const WORKERS = 4;
const LAST = 0x10ffff;
const UNMEASURED = 0xff;

type Widths = Uint8Array;

interface Formatter {
  name: string;
  command: string[];
  config: [string, object];
}

const FORMATTERS: Formatter[] = [
  {
    name: "@biomejs/biome",
    command: [join(bin, "biome"), "format", "--stdin-file-path", "a.ts"],
    config: [
      "biome.json",
      { formatter: { lineWidth: PRINT_WIDTH, indentStyle: "space" }, files: { maxSize: 1 << 30 } },
    ],
  },
  {
    name: "oxfmt",
    command: [join(bin, "oxfmt"), "--stdin-filepath", "a.ts"],
    config: [".oxfmtrc.json", { printWidth: PRINT_WIDTH }],
  },
];

function json(path: string) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function pinned(name: string): string {
  const wanted = json(join(root, "package.json")).devDependencies[name];
  const installed = json(join(root, "node_modules", name, "package.json")).version;
  if (installed !== wanted)
    throw new Error(`width-tables: ${name} ${installed} is installed, package.json pins ${wanted}`);

  return installed;
}

function measured(point: number): boolean {
  if (point >= 0x20 && point <= 0x7e) return false;
  if (point >= 0xd800 && point <= 0xdfff) return false;
  return point !== 0x09 && point !== 0x0a && point !== 0x0d;
}

const POINTS = Array.from({ length: LAST + 1 }, (_, point) => point).filter(measured);

function label(point: number): string {
  return `x${point.toString(16).padStart(6, "0")}`;
}

function statement(point: number, room: number): string {
  const frame = `${label(point)}("", 1);`;
  const pad = " ".repeat(PRINT_WIDTH - frame.length - room);
  return `${label(point)}("${pad}${String.fromCodePoint(point)}", 1);`;
}

function source(points: number[], rooms: number): string {
  const lines = points.flatMap((point) =>
    Array.from({ length: rooms }, (_, room) => statement(point, room)),
  );

  return `${lines.join("\n")}\n`;
}

async function format(
  formatter: Formatter,
  dir: string,
  name: string,
  text: string,
): Promise<string> {
  const input = join(dir, name);
  await Bun.write(input, text);

  const child = Bun.spawn(formatter.command, {
    cwd: dir,
    stdin: Bun.file(input),
    stdout: "pipe",
    stderr: "pipe",
  });

  const [out, err, code] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);

  if (code !== 0) throw new Error(`${formatter.name} exited ${code}: ${err.slice(0, 400)}`);
  return out;
}

function read(output: string, rooms: number, widths: Widths, formatter: Formatter): number[] {
  const fits = new Map<number, boolean[]>();
  const lines = output.split("\n");
  for (const [index, line] of lines.entries()) {
    const match = /^x([0-9a-f]{6})\(/.exec(line);
    if (!match) continue;

    const point = Number.parseInt(match[1]!, 16);
    const kept = line.endsWith(", 1);");
    const printed = kept ? line : (lines[index + 1] ?? "");
    if (!printed.includes(String.fromCodePoint(point)))
      throw new Error(`${formatter.name} rewrote U+${point.toString(16)}`);

    fits.set(point, [...(fits.get(point) ?? []), kept]);
  }

  const wider: number[] = [];
  for (const [point, kept] of fits) {
    if (kept.length !== rooms)
      throw new Error(`${formatter.name} lost a U+${point.toString(16)} line`);

    const width = kept.indexOf(true);
    if (width < 0) wider.push(point);
    else if (!kept.slice(width).every(Boolean))
      throw new Error(`${formatter.name} fit U+${point.toString(16)} unevenly`);
    else widths[point] = width;
  }

  return wider;
}

async function pool<T>(jobs: (() => Promise<T>)[]): Promise<T[]> {
  const results: T[] = [];
  let next = 0;
  let failed = false;
  const worker = async () => {
    while (!failed && next < jobs.length) {
      const index = next++;
      try {
        results[index] = await jobs[index]!();
      } catch (error) {
        failed = true;
        throw error;
      }
    }
  };

  const settled = await Promise.allSettled(Array.from({ length: WORKERS }, worker));
  const rejected = settled.find((outcome) => outcome.status === "rejected");
  if (rejected) throw rejected.reason;

  return results;
}

async function pass(
  formatter: Formatter,
  dir: string,
  points: number[],
  rooms: number,
  widths: Widths,
) {
  const jobs: (() => Promise<number[]>)[] = [];
  for (let first = 0; first < points.length; first += CHUNK) {
    const text = source(points.slice(first, first + CHUNK), rooms);
    const name = `${rooms}-${first}.ts`;
    jobs.push(() =>
      format(formatter, dir, name, text).then((output) => read(output, rooms, widths, formatter)),
    );
  }

  return (await pool(jobs)).flat();
}

async function oracle(formatter: Formatter): Promise<Widths> {
  const dir = mkdtempSync(join(tmpdir(), "width-tables-"));
  const widths: Widths = new Uint8Array(LAST + 1).fill(UNMEASURED);
  try {
    await Bun.write(join(dir, formatter.config[0]), JSON.stringify(formatter.config[1]));
    const wider = await pass(formatter, dir, POINTS, NARROW, widths);
    await pass(formatter, dir, wider, WIDE, widths);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }

  const missing = POINTS.filter((point) => widths[point] === UNMEASURED);
  if (missing.length > 0)
    throw new Error(`${formatter.name} left ${missing.length} scalars unmeasured`);

  return widths;
}

function prettier(): Widths {
  const widths: Widths = new Uint8Array(LAST + 1).fill(UNMEASURED);
  for (const point of POINTS) widths[point] = util.getStringWidth(String.fromCodePoint(point));
  return widths;
}

function pictographic(): Widths {
  const flags: Widths = new Uint8Array(LAST + 1).fill(UNMEASURED);
  for (const point of POINTS)
    flags[point] = /\p{Extended_Pictographic}/u.test(String.fromCodePoint(point)) ? 1 : 0;

  if (flags[0x2764] !== 1 || flags[0x1f600] !== 1 || flags[0x6f22] !== 0)
    throw new Error("width-tables: this runtime's Extended_Pictographic data is unusable");

  return flags;
}

function ranges(values: Widths, skip: number): [number, number, number][] {
  const found: [number, number, number][] = [];
  for (const point of POINTS) {
    const value = values[point]!;
    if (value === skip) continue;

    const last = found.at(-1);
    if (last && last[1] === point - 1 && last[2] === value) last[1] = point;
    else found.push([point, point, value]);
  }

  return found;
}

function char(point: number): string {
  return `'\\u{${point.toString(16)}}'`;
}

function widthTable(name: string, widths: Widths): string {
  const rows = ranges(widths, 1).map(
    ([first, last, width]) => `    (${char(first)}, ${char(last)}, ${width}),`,
  );

  return `pub const ${name}: &[(char, char, u8)] = &[\n${rows.join("\n")}\n];\n`;
}

function setTable(name: string, flags: Widths): string {
  const rows = ranges(flags, 0).map(([first, last]) => `    (${char(first)}, ${char(last)}),`);
  return `pub const ${name}: &[(char, char)] = &[\n${rows.join("\n")}\n];\n`;
}

const bun = readFileSync(join(root, ".bun-version"), "utf8").trim();
if (process.versions.bun !== bun)
  throw new Error(`width-tables: PICTOGRAPHIC comes from the runtime, run this with Bun ${bun}`);

const versions = {
  prettier: pinned("prettier"),
  biome: pinned("@biomejs/biome"),
  oxfmt: pinned("oxfmt"),
};

const prettierWidths = prettier();
const [biome, oxfmt] = await Promise.all(FORMATTERS.map(oracle));
const header = `// Generated by bun scripts/width-tables.ts from Prettier ${versions.prettier}, Biome ${versions.biome} and oxfmt ${versions.oxfmt}. Do not edit.\n`;

await Bun.write(
  target,
  [
    header,
    widthTable("PRETTIER", prettierWidths),
    widthTable("BIOME", biome!),
    widthTable("OXFMT", oxfmt!),
    setTable("PICTOGRAPHIC", pictographic()),
  ].join("\n"),
);

console.log(`width-tables: wrote ${target}`);
