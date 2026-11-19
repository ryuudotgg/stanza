import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { util } from "prettier";

const root = join(import.meta.dir, "..");
const bin = join(root, "node_modules", ".bin");
const target = join(root, "crates", "stanza", "src", "engine", "widths.rs");
const sequences = join(root, "crates", "stanza", "src", "engine", "sequences.json");

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

const SEQUENCES = [
  "👍🏽",
  "👋🏻",
  "👨\u200d👩\u200d👧",
  "🧑🏽\u200d💻",
  "❤\ufe0f",
  "❤\ufe0f\u200d🔥",
  "1\ufe0f\u20e3",
  "#\ufe0f\u20e3",
  "🇯🇵",
  "🇯",
  "🏴\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}",
  "🏴\u{e0067}",
  "😀\u200d",
  "😀😀",
  "漢\u200d字",
  "x\ufe0f",
  "e\u0301",
  "भारत",
  "กั",
];

type Probe = [id: number, text: string];

const SCALARS: Probe[] = POINTS.map((point) => [point, String.fromCodePoint(point)]);
const SEQUENCE_PROBES: Probe[] = SEQUENCES.map((text, index) => [LAST + 1 + index, text]);

function label(id: number): string {
  return `x${id.toString(16).padStart(6, "0")}`;
}

function statement([id, text]: Probe, room: number): string {
  const frame = `${label(id)}("", 1);`;
  const pad = " ".repeat(PRINT_WIDTH - frame.length - room);
  return `${label(id)}("${pad}${text}", 1);`;
}

function source(probes: Probe[], rooms: number): string {
  const lines = probes.flatMap((probe) =>
    Array.from({ length: rooms }, (_, room) => statement(probe, room)),
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

function read(
  output: string,
  rooms: number,
  texts: Map<number, string>,
  widths: Map<number, number>,
  formatter: Formatter,
): Probe[] {
  const fits = new Map<number, boolean[]>();
  const lines = output.split("\n");
  for (const [index, line] of lines.entries()) {
    const match = /^x([0-9a-f]{6})\(/.exec(line);
    if (!match) continue;

    const id = Number.parseInt(match[1]!, 16);
    const kept = line.endsWith(", 1);");
    const printed = kept ? line : (lines[index + 1] ?? "");
    if (!printed.includes(texts.get(id)!))
      throw new Error(`${formatter.name} rewrote probe ${id.toString(16)}`);

    fits.set(id, [...(fits.get(id) ?? []), kept]);
  }

  const wider: Probe[] = [];
  for (const [id, kept] of fits) {
    if (kept.length !== rooms)
      throw new Error(`${formatter.name} lost a probe ${id.toString(16)} line`);

    const width = kept.indexOf(true);
    if (width < 0) wider.push([id, texts.get(id)!]);
    else if (!kept.slice(width).every(Boolean))
      throw new Error(`${formatter.name} fit probe ${id.toString(16)} unevenly`);
    else widths.set(id, width);
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
  probes: Probe[],
  rooms: number,
  widths: Map<number, number>,
) {
  const texts = new Map(probes);
  const jobs: (() => Promise<Probe[]>)[] = [];
  for (let first = 0; first < probes.length; first += CHUNK) {
    const text = source(probes.slice(first, first + CHUNK), rooms);
    const name = `${rooms}-${first}.ts`;
    jobs.push(() =>
      format(formatter, dir, name, text).then((output) =>
        read(output, rooms, texts, widths, formatter),
      ),
    );
  }

  return (await pool(jobs)).flat();
}

async function oracle(formatter: Formatter): Promise<Map<number, number>> {
  const dir = mkdtempSync(join(tmpdir(), "width-tables-"));
  const widths = new Map<number, number>();
  const probes = [...SCALARS, ...SEQUENCE_PROBES];
  try {
    await Bun.write(join(dir, formatter.config[0]), JSON.stringify(formatter.config[1]));
    const wider = await pass(formatter, dir, probes, NARROW, widths);
    await pass(formatter, dir, wider, WIDE, widths);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }

  const missing = probes.filter(([id]) => !widths.has(id));
  if (missing.length > 0)
    throw new Error(`${formatter.name} left ${missing.length} probes unmeasured`);

  return widths;
}

function scalars(measured: Map<number, number>): Widths {
  const widths: Widths = new Uint8Array(LAST + 1).fill(UNMEASURED);
  for (const point of POINTS) widths[point] = measured.get(point)!;
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

const [biome, oxfmt] = await Promise.all(FORMATTERS.map(oracle));
const header = `// Generated by bun scripts/width-tables.ts from Prettier ${versions.prettier}, Biome ${versions.biome} and oxfmt ${versions.oxfmt}. Do not edit.\n`;

await Bun.write(
  target,
  [
    header,
    widthTable("PRETTIER", prettier()),
    widthTable("BIOME", scalars(biome!)),
    widthTable("OXFMT", scalars(oxfmt!)),
    setTable("PICTOGRAPHIC", pictographic()),
  ].join("\n"),
);

const samples = SEQUENCE_PROBES.map(([id, text]) => ({
  text,
  prettier: util.getStringWidth(text),
  biome: biome!.get(id),
  oxfmt: oxfmt!.get(id),
}));

await Bun.write(sequences, `${JSON.stringify(samples, null, 2)}\n`);
console.log(`width-tables: wrote ${target} and ${sequences}`);
