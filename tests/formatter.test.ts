import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { batchClient, type BatchResponse } from "../scripts/batch.ts";
import { batchCommand } from "./support.ts";

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

interface Allowed {
  rule: string;
  line: string;
}

interface Round {
  changed: string[];
  unexpected: string[];
}

function pin(dir: string, formatter: Formatter): void {
  writeFileSync(
    join(dir, "package.json"),
    JSON.stringify({ devDependencies: { [formatter.pkg]: pins[formatter.pkg] } }),
  );
}

async function runFormatter(
  formatter: Formatter,
  sources: Map<string, string>,
): Promise<Map<string, string>> {
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

    return new Map(
      [...sources.keys()].map((path) => [path, readFileSync(join(dir, path), "utf8")]),
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function allowedFindings(path: string): Allowed[] {
  const findings = join(fixtures, path.replace(/\.after\.[^.]+$/, ".findings"));
  if (!existsSync(findings)) return [];

  const lines = readFileSync(join(fixtures, path), "utf8").split(/\r?\n/);
  return readFileSync(findings, "utf8")
    .trim()
    .split(/\r?\n/)
    .filter(Boolean)
    .map((row) => {
      const match = /^(\d+):\d+ (\S+)$/.exec(row);
      const line = match ? lines[Number(match[1]) - 1] : undefined;
      if (line === undefined) throw new Error(`${findings}: no line of ${path} for row "${row}"`);

      return { rule: match![2]!, line: line.trim() };
    });
}

const client = batchClient(batchCommand());
function ask(path: string, text: string, mode: "check" | "fix"): Promise<BatchResponse> {
  return client.ask({ path, text, mode, braces: null, changedLines: null, explain: [] });
}

async function unexpected(file: string, path: string, text: string): Promise<string[]> {
  const { error, findings } = await ask(file, text, "check");
  if (error !== null) return [`${path} stanza --check: ${error}`];

  const lines = text.split(/\r?\n/);
  const allowed = allowedFindings(path);
  return findings
    .filter((finding) => {
      const line = lines[finding.line - 1]?.trim();
      const index =
        finding.rule === "parse"
          ? -1
          : allowed.findIndex((entry) => entry.rule === finding.rule && entry.line === line);

      if (index === -1) return true;

      allowed.splice(index, 1);
      return false;
    })
    .map((finding) => `${path}:${finding.line}:${finding.col} ${finding.rule}`);
}

async function runRound(formatter: Formatter, dir: string, paths: string[]): Promise<Round[]> {
  const sources = new Map(paths.map((path) => [path, readFileSync(join(dir, path), "utf8")]));
  const outputs = await runFormatter(formatter, sources);

  return Promise.all(
    paths.map(async (path) => {
      const file = join(dir, path);
      const formatted = outputs.get(path)!;
      const changed =
        formatted === sources.get(path) ? [] : [`${path} changed by ${formatter.name}`];

      const fixed = await ask(file, formatted, "fix");
      const text = fixed.text ?? formatted;
      writeFileSync(file, text);

      const parsed =
        fixed.error === null && !fixed.findings.some((finding) => finding.rule === "parse");

      if (!parsed)
        return { changed, unexpected: [`${path} stanza --fix: ${fixed.error ?? "parse finding"}`] };

      if (text !== formatted) changed.push(`${path} changed by stanza --fix`);
      return { changed, unexpected: await unexpected(file, path, text) };
    }),
  );
}

const fixtureFiles = readdirSync(fixtures, { withFileTypes: true })
  .filter((entry) => entry.isDirectory())
  .map((entry) => entry.name)
  .sort()
  .map((name) => ({
    name,
    files: readdirSync(join(fixtures, name))
      .filter((file) => /\.after\.[jt]sx?$/.test(file))
      .sort(),
  }));

const prepared: string[] = [];
const records = new Map<string, [Round, Round]>();

beforeAll(async () => {
  await Promise.all(
    formatters.map(async (formatter) => {
      const paths = fixtureFiles
        .flatMap(({ name, files }) => files.map((file) => `${name}/${file}`))
        .filter((path) => !formatter.rejects.includes(path));

      const dir = mkdtempSync(join(tmpdir(), `stanza-${formatter.name}-`));
      prepared.push(dir);

      for (const { name } of fixtureFiles)
        cpSync(join(fixtures, name), join(dir, name), { recursive: true });

      pin(dir, formatter);

      const first = await runRound(formatter, dir, paths);
      const second = await runRound(formatter, dir, paths);
      for (const [index, path] of paths.entries())
        records.set(`${formatter.name} ${path}`, [first[index]!, second[index]!]);
    }),
  );
}, 60_000);

test("the fixture copies hold no .oxfmtrc.json", () => {
  expect(prepared).toHaveLength(formatters.length);
  expect(prepared.filter((dir) => existsSync(join(dir, ".oxfmtrc.json")))).toEqual([]);
});

afterAll(async () => {
  for (const dir of prepared) rmSync(dir, { recursive: true, force: true });
  expect(await client.close()).toEqual([]);
});

for (const formatter of formatters)
  for (const { name, files } of fixtureFiles) {
    if (files.length === 0) continue;

    describe(`${name}: stanza and ${formatter.name} settle in one round`, () => {
      for (const file of files.filter((file) => !formatter.rejects.includes(`${name}/${file}`)))
        test(file, () => {
          const [first, second] = records.get(`${formatter.name} ${name}/${file}`)!;
          const label = (round: number) => (entry: string) =>
            `${formatter.name} round ${round} ${entry}`;

          expect([
            ...first.unexpected.map(label(1)),
            ...second.changed.map(label(2)),
            ...second.unexpected.map(label(2)),
          ]).toEqual([]);
        });
    });
  }
