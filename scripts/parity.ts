import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  lstatSync,
  readdirSync,
  readFileSync,
  readlinkSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { availableParallelism, homedir } from "node:os";
import { basename, dirname, join, relative, resolve } from "node:path";
import { parseArgs } from "node:util";
import { beginInvocation } from "../src/directories.ts";
import { RULES } from "../src/engine/rules.ts";
import { collectFiles } from "../src/files.ts";

const root = join(import.meta.dir, "..");

const MODES = ["check", "json", "fix", "stdin", "explain"] as const;
type Mode = (typeof MODES)[number];

const LAYOUTS = ["isolated", "in-place"] as const;
type Layout = (typeof LAYOUTS)[number];

const CORPORA = ["Kingshot", "Stanza", "meisai", "F&S", "caasx"].map((name) =>
  join(homedir(), "Projects", name),
);

const BATCH = 100;
const SPAWN_TIMEOUT_MS = 120_000;
const EXPLAIN_SAMPLE = 5;
const BATCH_KEY = "";
const CLEAN_EXITS = new Set(["exit 0", "exit 1", "exit 2"]);

class UsageError extends Error {}

interface Entry {
  file: string;
  pattern?: string;
}

interface Settings {
  reference: string[];
  candidate: string;
  inputs: string[];
  modes: Mode[];
  rules: ReadonlySet<string> | undefined;
  braces: string[];
  layouts: Layout[];
  mutate: boolean;
  seed: number;
  cleanOnly: boolean;
  explainAll: boolean;
  dumps: string[];
  entries: Entry[];
}

interface Context extends Settings {
  scratch: string;
  crashes: Difference[];
}

interface Tree {
  label: string;
  root: string;
  files: string[];
}

interface Arrangement {
  layout: Layout;
  dir: string;
  candidateDir: string;
  paths: string[];
  shown: Map<string, string>;
}

interface Where {
  arrangement: Arrangement;
  mode: string;
  batch: string[];
}

interface Outcome {
  code: string;
  stdout: string;
  stderr: string;
  bytes: string;
  git: string[];
}

interface Difference {
  path: string;
  layout: Layout;
  mode: string;
  reference: string;
  candidate: string;
}

interface Finding {
  path: string;
  line: number;
  rule: string;
}

type Split = Map<string, string[]>;

const usage = `Usage: bun scripts/parity.ts --candidate <binary> [options] [paths...]

  --candidate <binary>   the binary compared against the reference
  --reference <command>  the reference command, default "bun src/cli.ts"
  --modes <list>         comma separated: ${MODES.join(", ")}; default all
  --rules <list>         only findings of these rules in check, json and explain
  --isolate              copy each file alone into scratch, so neither side reads config
  --no-braces            pass --no-braces to both sides in every mode
  --corpora              add ${CORPORA.map((path) => basename(path)).join(", ")} under ~/Projects
  --mutate               add a seeded mutated copy of every input
  --seed <n>             the mutation seed, default 1
  --clean-only           keep only files the reference reports nothing for
  --explain-all          explain every finding line, not five per file
  --dump <name>          diff stanza-dev <name> beside the candidate against scripts/dev/<name>.ts
  --manifest <file>      take rules, modes, layouts and noBraces from the manifest and run its entries first

Paths default to tests/fixtures when no path and no --corpora is given.`;

function list(value: string | undefined): string[] | undefined {
  return value
    ?.split(",")
    .map((item) => item.trim())
    .filter((item) => item !== "");
}

function oneOf<T extends string>(values: readonly T[], items: string[], what: string): T[] {
  const unknown = items.filter((item) => !(values as readonly string[]).includes(item));
  if (unknown.length > 0) throw new UsageError(`unknown ${what}: ${unknown.join(", ")}`);
  return [...new Set(items)] as T[];
}

const FINDING_RULES = new Set([...Object.keys(RULES), "parse", "error", "write"]);

function knownRules(items: string[]): ReadonlySet<string> | undefined {
  const unknown = items.filter((item) => !FINDING_RULES.has(item));
  if (unknown.length > 0) throw new UsageError(`unknown rule: ${unknown.join(", ")}`);
  return items.length > 0 ? new Set(items) : undefined;
}

function command(text: string): string[] {
  const tokens = text
    .trim()
    .split(/\s+/)
    .filter((token) => token !== "");

  if (tokens.length === 0) throw new UsageError("--reference is empty");
  return tokens.map((token) =>
    !token.startsWith("-") && token.includes("/") && existsSync(token) ? resolve(token) : token,
  );
}

function strings(value: unknown, path: string, key: string): string[] {
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string"))
    throw new UsageError(`${path}: ${key} must be an array of strings`);

  return value as string[];
}

function readManifest(path: string) {
  if (!existsSync(path)) throw new UsageError(`no such manifest: ${path}`);

  let manifest: Record<string, unknown>;
  try {
    manifest = JSON.parse(readFileSync(path, "utf8")) as Record<string, unknown>;
  } catch (error: unknown) {
    throw new UsageError(`${path}: ${(error as Error).message}`);
  }

  if (!Array.isArray(manifest.entries) || !Array.isArray(manifest.replaced))
    throw new UsageError(`${path}: entries and replaced must be arrays`);

  if (manifest.noBraces !== undefined && typeof manifest.noBraces !== "boolean")
    throw new UsageError(`${path}: noBraces must be a boolean`);

  const entries = manifest.entries as Entry[];
  for (const entry of entries)
    if (typeof entry.file !== "string" || !existsSync(join(root, entry.file)))
      throw new UsageError(`${path}: no such test file: ${String(entry.file)}`);

  return {
    entries,
    rules: strings(manifest.rules, path, "rules"),
    modes: strings(manifest.modes, path, "modes"),
    layouts: strings(manifest.layouts, path, "layouts"),
    noBraces: manifest.noBraces === true,
  };
}

function filterable(mode: Mode): boolean {
  return mode !== "fix" && mode !== "stdin";
}

function parseSettings(args: string[]): Settings {
  const { values, positionals } = parseArgs({
    args,
    allowPositionals: true,
    options: {
      candidate: { type: "string" },
      reference: { type: "string" },
      modes: { type: "string" },
      rules: { type: "string" },
      isolate: { type: "boolean", default: false },
      "no-braces": { type: "boolean", default: false },
      corpora: { type: "boolean", default: false },
      mutate: { type: "boolean", default: false },
      seed: { type: "string", default: "1" },
      "clean-only": { type: "boolean", default: false },
      "explain-all": { type: "boolean", default: false },
      dump: { type: "string", multiple: true, default: [] },
      manifest: { type: "string" },
    },
  });

  if (!values.candidate) throw new UsageError("--candidate is required");

  const candidate = resolve(values.candidate);
  if (!existsSync(candidate)) throw new UsageError(`no such candidate: ${values.candidate}`);

  const seed = Number(values.seed);
  if (!Number.isSafeInteger(seed)) throw new UsageError(`--seed takes an integer: ${values.seed}`);

  const manifest = values.manifest ? readManifest(resolve(values.manifest)) : undefined;
  if (manifest && (values.modes || values.rules || values.isolate || values["no-braces"]))
    throw new UsageError(
      "--manifest sets modes, rules, layouts and braces, so it takes none of those flags",
    );

  const rules = knownRules(manifest?.rules ?? list(values.rules) ?? []);
  const layouts = oneOf(
    LAYOUTS,
    manifest?.layouts ?? [values.isolate ? "isolated" : "in-place"],
    "layout",
  );

  const everyMode =
    values.dump.length > 0 ? [] : MODES.filter((mode) => !rules || filterable(mode));

  const modes = oneOf(MODES, manifest?.modes ?? list(values.modes) ?? everyMode, "mode");
  if ((modes.length > 0 || values.dump.length > 0) && layouts.length === 0)
    throw new UsageError("modes need at least one layout");

  const unfiltered = modes.filter((mode) => !filterable(mode));
  if (rules && unfiltered.length > 0)
    throw new UsageError(`${unfiltered.join(" and ")} compare only without a rule filter`);

  for (const name of values.dump) {
    if (!existsSync(join(root, "scripts", "dev", `${name}.ts`)))
      throw new UsageError(`no dev dump scripts/dev/${name}.ts`);

    if (!existsSync(join(dirname(candidate), "stanza-dev")))
      throw new UsageError(`no stanza-dev beside ${values.candidate}`);
  }

  const inputs = [...positionals, ...(values.corpora ? CORPORA : [])];
  if (inputs.length === 0) inputs.push(join(root, "tests", "fixtures"));

  for (const input of inputs)
    if (!existsSync(input) || !statSync(input).isDirectory())
      throw new UsageError(`no such directory: ${input}`);

  return {
    // Bun loads a bunfig.toml or .env from the cwd, so a corpus could configure or run code in the reference.
    reference: values.reference
      ? command(values.reference)
      : ["bun", "--no-env-file", "--config=/dev/null", join(root, "src", "cli.ts")],
    candidate,
    inputs: inputs.map((input) => resolve(input)),
    modes,
    rules,
    braces: (manifest?.noBraces ?? values["no-braces"]) ? ["--no-braces"] : [],
    layouts,
    mutate: values.mutate,
    seed,
    cleanOnly: values["clean-only"],
    explainAll: values["explain-all"],
    dumps: values.dump,
    entries: manifest?.entries ?? [],
  };
}

for (const key of Object.keys(process.env)) if (key.startsWith("GIT_")) delete process.env[key];

const env = Object.fromEntries(
  Object.entries(process.env).filter(([key]) => key !== "FORCE_COLOR"),
);

const realGit = Bun.which("git") ?? "git";

const slots = availableParallelism();
const waiting: (() => void)[] = [];

let running = 0;
async function slot<T>(task: () => Promise<T>): Promise<T> {
  if (running < slots) running++;
  else await new Promise<void>((wake) => waiting.push(wake));

  try {
    return await task();
  } finally {
    const next = waiting.shift();
    if (next) next();
    else running--;
  }
}

function gitShim(dir: string): string {
  const log = join(dir, "log");
  writeFileSync(
    join(dir, "git"),
    `#!/bin/sh\n{ printf git; for arg in "$@"; do printf ' [%s]' "$arg"; done; printf '\\n'; } >> '${log}'\nexec '${realGit}' "$@"\n`,
    { mode: 0o755 },
  );

  return log;
}

function digest(...parts: Uint8Array[]): string {
  const hasher = new Bun.CryptoHasher("sha1");
  for (const part of parts) hasher.update(part);
  return hasher.digest("hex");
}

function withoutCwd(bytes: Uint8Array, cwd: string): string {
  return Buffer.from(bytes)
    .toString("latin1")
    .replaceAll(Buffer.from(cwd).toString("latin1"), "<cwd>");
}

function spawn(
  context: Context,
  argv: string[],
  cwd: string,
  stdin?: Uint8Array,
): Promise<Outcome> {
  return slot(async () => {
    const shim = mkdtempSync(join(context.scratch, "shim-"));
    const log = gitShim(shim);
    const child = Bun.spawn(argv, {
      cwd,
      env: { ...env, PATH: `${shim}:${env.PATH ?? ""}` },
      stdin: stdin ? new Blob([stdin]) : "ignore",
      stdout: "pipe",
      stderr: "pipe",
      timeout: SPAWN_TIMEOUT_MS,
    });

    const [stdout, stderr] = await Promise.all([
      new Response(child.stdout).bytes(),
      new Response(child.stderr).bytes(),
    ]);

    await child.exited;

    const git = existsSync(log) ? withoutCwd(readFileSync(log), cwd).split("\n") : [];
    rmSync(shim, { recursive: true, force: true });

    const [out, err] = [withoutCwd(stdout, cwd), withoutCwd(stderr, cwd)];
    const decoder = new TextDecoder();
    return {
      code: child.signalCode ?? `exit ${child.exitCode}`,
      stdout: decoder.decode(Buffer.from(out, "latin1")),
      stderr: decoder.decode(Buffer.from(err, "latin1")),
      bytes: digest(Buffer.from(out, "latin1"), Buffer.from(err, "latin1")),
      git: git.filter((line) => line !== ""),
    };
  });
}

function shownPath(where: Where, key: string): string {
  const { arrangement, batch } = where;
  if (key !== BATCH_KEY) return arrangement.shown.get(key) ?? key;

  const first = arrangement.shown.get(batch[0]!) ?? batch[0]!;
  return batch.length > 1 ? `${first} and ${batch.length - 1} more` : first;
}

async function pair(
  context: Context,
  where: Where,
  tail: string[],
  dirs: { reference: string; candidate: string },
  stdin?: Uint8Array,
): Promise<[Outcome, Outcome]> {
  const [reference, candidate] = await Promise.all([
    spawn(context, [...context.reference, ...tail], dirs.reference, stdin),
    spawn(context, [context.candidate, ...tail], dirs.candidate, stdin),
  ]);

  if (!CLEAN_EXITS.has(reference.code) || !CLEAN_EXITS.has(candidate.code))
    context.crashes.push({
      path: shownPath(where, BATCH_KEY),
      layout: where.arrangement.layout,
      mode: `${where.mode} crash`,
      reference: `${reference.code} ${reference.stderr.split("\n")[0]}`,
      candidate: `${candidate.code} ${candidate.stderr.split("\n")[0]}`,
    });

  return [reference, candidate];
}

function sides(arrangement: Arrangement): { reference: string; candidate: string } {
  return { reference: arrangement.dir, candidate: arrangement.candidateDir };
}

function must(argv: string[], cwd?: string): void {
  const result = Bun.spawnSync(argv, { cwd, env, stderr: "pipe" });
  if (!result.success) throw new Error(`${argv.join(" ")} failed: ${result.stderr.toString()}`);
}

function gitToplevel(dir: string): boolean {
  const result = Bun.spawnSync(["git", "-C", dir, "rev-parse", "--show-toplevel"], {
    env,
  });

  return result.success && realpathSync(result.stdout.toString().trim()) === realpathSync(dir);
}

function exportInput(input: string, into: string): void {
  mkdirSync(into, { recursive: true });

  if (!gitToplevel(input)) {
    cpSync(input, into, {
      recursive: true,
      verbatimSymlinks: true,
      filter: (path) => basename(path) !== ".git",
    });

    return;
  }

  const archive = `${into}.tar`;
  must(["git", "-C", input, "archive", "--output", archive, "HEAD"]);
  must(["tar", "-xf", archive, "-C", into]);
  rmSync(archive);
}

function snapshotRepository(dir: string): void {
  if (!existsSync(join(dir, ".git"))) must(["git", "init", "-q"], dir);

  must(["git", "add", "-A"], dir);
  must(
    [
      "git",
      "-c",
      "user.name=parity",
      "-c",
      "user.email=parity@stanza.invalid",
      "-c",
      "commit.gpgsign=false",
      "commit",
      "-q",
      "--no-verify",
      "--allow-empty",
      "-m",
      "parity",
    ],
    dir,
  );
}

function select(dir: string): string[] {
  beginInvocation();

  const selection = collectFiles([dir], dir, true);
  if (selection.errors.length > 0) throw new Error(selection.errors.join("\n"));
  return selection.files.map((file) => relative(dir, file));
}

function hash(text: string): number {
  let value = 0x811c9dc5;
  for (let index = 0; index < text.length; index++)
    value = Math.imul(value ^ text.charCodeAt(index), 0x01000193);

  return value >>> 0;
}

function seeded(seed: number): () => number {
  let state = seed;
  return () => {
    state = (state + 0x6d2b79f5) | 0;

    let value = Math.imul(state ^ (state >>> 15), 1 | state);
    value = (value + Math.imul(value ^ (value >>> 7), 61 | value)) ^ value;
    return ((value ^ (value >>> 14)) >>> 0) / 4294967296;
  };
}

const braceless =
  /^(\s*)((?:\} )?(?:else )?(?:if|for|while) \(.*\)) ((?:return|throw|break|continue)\b[^{}]*;)\s*$/;

const elseLater = /\belse\b.*;/;

function indentOf(line: string | undefined): string {
  return line?.match(/^[ \t]*/)?.[0] ?? "";
}

function mutate(text: string, random: () => number): string {
  const eol = text.includes("\r\n") ? "\r\n" : "\n";
  const lines = text.split(eol);
  const out: string[] = [];
  for (const [index, line] of lines.entries()) {
    if (line.trim() === "" && index < lines.length - 1 && random() < 0.3) continue;
    if (line.trim() === "}" && random() < 0.1) out.push("", `${indentOf(out.at(-1))}// parity`);

    const body = elseLater.test(line) ? null : line.match(braceless);
    out.push(body && random() < 0.5 ? `${body[1]}${body[2]} { ${body[3]} }` : line);
    if (index === lines.length - 1) break;

    const next = lines[index + 1];
    if (line.trimEnd().endsWith("{") && random() < 0.1) out.push(`${indentOf(next)}// parity`, "");
    if (random() < 0.08) out.push("");
    if (random() < 0.03) out.push([" ", "\t", "  \t "][Math.floor(random() * 3)]!);
  }

  return out.join(eol);
}

function prepareTrees(context: Context): Tree[] {
  const trees: Tree[] = [];
  const labels = new Set<string>();
  for (const input of context.inputs) {
    let label = basename(input);
    for (let suffix = 2; labels.has(label); suffix++) label = `${basename(input)}-${suffix}`;

    labels.add(label);

    const dir = join(context.scratch, "trees", label);
    exportInput(input, dir);
    snapshotRepository(dir);

    const files = select(dir);
    trees.push({ label, root: dir, files });
    if (!context.mutate) continue;

    const mutated = join(context.scratch, "trees", `${label}~mutated`);
    cpSync(dir, mutated, { recursive: true, verbatimSymlinks: true });

    for (const file of files) {
      const path = join(mutated, file);
      const random = seeded(hash(`${context.seed}:${file}`));
      writeFileSync(path, mutate(readFileSync(path, "latin1"), random), "latin1");
    }

    snapshotRepository(mutated);
    trees.push({ label: `${label}~mutated`, root: mutated, files });
  }

  return trees;
}

function withCandidateCopy(arrangement: Omit<Arrangement, "candidateDir">): Arrangement {
  const candidateDir = `${arrangement.dir}~candidate`;
  cpSync(arrangement.dir, candidateDir, { recursive: true, verbatimSymlinks: true });
  return { ...arrangement, candidateDir };
}

function neutralName(file: string): string {
  return `source${basename(file).match(/(?:\.d)?\.[^.]+$/)?.[0] ?? ""}`;
}

function arrange(tree: Tree, layout: Layout, scratch: string): Arrangement {
  if (layout === "in-place") {
    const shown = new Map(tree.files.map((file) => [file, `${tree.label}/${file}`]));
    return withCandidateCopy({ layout, dir: tree.root, paths: tree.files, shown });
  }

  const dir = join(scratch, "isolated", tree.label);
  const shown = new Map<string, string>();
  const paths = tree.files.map((file, index) => {
    const path = `${index}/${neutralName(file)}`;
    mkdirSync(join(dir, String(index)), { recursive: true });
    cpSync(join(tree.root, file), join(dir, path));
    shown.set(path, `${tree.label}/${file}`);
    return path;
  });

  return withCandidateCopy({ layout, dir, paths, shown });
}

function batches(paths: string[]): string[][] {
  const groups: string[][] = [];
  for (let start = 0; start < paths.length; start += BATCH)
    groups.push(paths.slice(start, start + BATCH));

  return groups;
}

function ending(outcome: Outcome): string[] {
  return [
    outcome.code,
    ...outcome.stderr.split("\n").map((line) => `stderr: ${line}`),
    ...outcome.git.map((line) => `spawned ${line}`),
    `bytes ${outcome.bytes}`,
  ];
}

function failure(outcome: Outcome): string[] {
  return [
    outcome.code === "exit 2" ? "exit 2" : "exit 0 or 1",
    ...outcome.stderr.split("\n").map((line) => `stderr: ${line}`),
  ];
}

function outcomeLines(outcome: Outcome): string[] {
  return [...outcome.stdout.split("\n"), ...ending(outcome)];
}

function whole(outcome: Outcome): Split {
  return new Map([[BATCH_KEY, outcomeLines(outcome)]]);
}

const findingLine = /^(.*?):(\d+):(\d+) (\S+) /;
function splitLines(
  outcome: Outcome,
  paths: ReadonlySet<string>,
  rules?: ReadonlySet<string>,
): Split {
  const split: Split = new Map();
  const add = (key: string, line: string) => {
    const lines = split.get(key);
    if (lines) lines.push(line);
    else split.set(key, [line]);
  };

  for (const line of outcome.stdout.split("\n")) {
    const match = line.match(findingLine);
    if (rules) {
      if (match && rules.has(match[4]!)) add(match[1]!, line);
      continue;
    }

    add(match && paths.has(match[1]!) ? match[1]! : BATCH_KEY, line);
  }

  for (const line of rules ? failure(outcome) : ending(outcome)) add(BATCH_KEY, line);
  return split;
}

function parseFindings(stdout: string): Finding[] | undefined {
  try {
    const parsed: unknown = JSON.parse(stdout);
    return Array.isArray(parsed) ? (parsed as Finding[]) : undefined;
  } catch {
    return undefined;
  }
}

function splitJson(outcome: Outcome, rules?: ReadonlySet<string>): Split {
  const findings = parseFindings(outcome.stdout);
  if (!findings) return whole(outcome);

  const split: Split = new Map();
  for (const finding of findings) {
    if (rules && !rules.has(finding.rule)) continue;

    const lines = split.get(finding.path) ?? [];
    lines.push(JSON.stringify(finding));
    split.set(finding.path, lines);
  }

  split.set(BATCH_KEY, rules ? failure(outcome) : ending(outcome));
  return split;
}

const WINDOW = 120;
function shownLine(line: string | undefined, at: number): string {
  if (line === undefined) return "(nothing)";
  if (line.length <= WINDOW) return JSON.stringify(line);

  const start = Math.max(0, Math.min(at - WINDOW / 4, line.length - WINDOW));
  const clipped = JSON.stringify(line.slice(start, start + WINDOW));
  return `${start > 0 ? "..." : ""}${clipped}${start + WINDOW < line.length ? "..." : ""}`;
}

function firstDifference(left: string[], right: string[]): [string, string] | undefined {
  for (let index = 0; index < Math.max(left.length, right.length); index++) {
    const [one, other] = [left[index], right[index]];
    if (one === other) continue;

    let at = 0;
    while (one !== undefined && other !== undefined && one[at] === other[at]) at++;

    return [shownLine(one, at), shownLine(other, at)];
  }

  return undefined;
}

function compare(where: Where, reference: Split, candidate: Split): Difference[] {
  const differences: Difference[] = [];
  for (const key of new Set([...reference.keys(), ...candidate.keys()])) {
    const found = firstDifference(reference.get(key) ?? [], candidate.get(key) ?? []);
    if (!found) continue;

    differences.push({
      path: shownPath(where, key),
      layout: where.arrangement.layout,
      mode: where.mode,
      reference: found[0],
      candidate: found[1],
    });
  }

  return differences;
}

function compareBatch(
  where: Where,
  split: [Split, Split],
  outcomes: [Outcome, Outcome],
  filtered: boolean,
) {
  const differences = compare(where, ...split);
  if (differences.length > 0 || filtered) return differences;
  return compare(where, whole(outcomes[0]), whole(outcomes[1]));
}

async function referenceFindings(context: Context, arrangement: Arrangement) {
  const found = new Map<string, Finding[]>(arrangement.paths.map((path) => [path, []]));
  const outcomes = await Promise.all(
    batches(arrangement.paths).map((batch) =>
      spawn(
        context,
        [...context.reference, "--check", "--json", ...context.braces, "--", ...batch],
        arrangement.dir,
      ),
    ),
  );

  for (const [index, outcome] of outcomes.entries()) {
    const findings = parseFindings(outcome.stdout);
    if (!findings)
      throw new Error(
        `the reference printed no findings JSON for ${arrangement.shown.get(batches(arrangement.paths)[index]![0]!)}: ${outcome.code} ${outcome.stderr.split("\n")[0]}`,
      );

    for (const finding of findings) found.get(finding.path)?.push(finding);
  }

  return found;
}

async function checkMode(context: Context, arrangement: Arrangement): Promise<Difference[]> {
  const results = await Promise.all(
    batches(arrangement.paths).map(async (batch) => {
      const where = { arrangement, mode: "check", batch };
      const outcomes = await pair(
        context,
        where,
        ["--check", ...context.braces, "--", ...batch],
        sides(arrangement),
      );

      const paths = new Set(batch);
      const split: [Split, Split] = [
        splitLines(outcomes[0], paths, context.rules),
        splitLines(outcomes[1], paths, context.rules),
      ];

      return compareBatch(where, split, outcomes, context.rules !== undefined);
    }),
  );

  return results.flat();
}

async function jsonMode(context: Context, arrangement: Arrangement): Promise<Difference[]> {
  const results = await Promise.all(
    batches(arrangement.paths).map(async (batch) => {
      const where = { arrangement, mode: "json", batch };
      const tail = ["--check", "--json", ...context.braces, "--", ...batch];
      const outcomes = await pair(context, where, tail, sides(arrangement));
      const split: [Split, Split] = [
        splitJson(outcomes[0], context.rules),
        splitJson(outcomes[1], context.rules),
      ];

      return compareBatch(where, split, outcomes, context.rules !== undefined);
    }),
  );

  return results.flat();
}

function treeFiles(dir: string): string[] {
  return readdirSync(dir, { recursive: true, withFileTypes: true })
    .filter((entry) => !entry.isDirectory())
    .map((entry) => relative(dir, join(entry.parentPath, entry.name)))
    .filter((path) => path !== ".git" && !path.startsWith(".git/"))
    .sort();
}

function entryLine(dir: string, path: string): string {
  const file = join(dir, path);
  const stat = lstatSync(file);
  if (stat.isSymbolicLink()) return `${path} link ${readlinkSync(file)}`;

  return `${path} ${stat.isFile() ? "file" : "other"} ${(stat.mode & 0o7777).toString(8)}`;
}

function writtenTree(dir: string, paths: string[]): Split {
  const selected = new Set(paths);
  const listed = treeFiles(dir);
  const split: Split = new Map([[BATCH_KEY, listed.map((path) => entryLine(dir, path))]]);
  for (const path of listed) {
    if (!lstatSync(join(dir, path)).isFile()) continue;

    const bytes = readFileSync(join(dir, path));
    const shown = selected.has(path) ? bytes.toString("utf8").split("\n") : [];
    split.set(path, [...shown, `bytes ${digest(bytes)}`]);
  }

  for (const path of paths) if (!split.has(path)) split.set(path, ["missing"]);
  return split;
}

async function fixMode(context: Context, arrangement: Arrangement): Promise<Difference[]> {
  const copies = mkdtempSync(join(context.scratch, "fix-"));
  const sides = { reference: join(copies, "reference"), candidate: join(copies, "candidate") };
  cpSync(arrangement.dir, sides.reference, { recursive: true, verbatimSymlinks: true });
  cpSync(arrangement.dir, sides.candidate, { recursive: true, verbatimSymlinks: true });

  const results = await Promise.all(
    batches(arrangement.paths).map(async (batch) => {
      const where = { arrangement, mode: "fix", batch };
      const outcomes = await pair(
        context,
        where,
        ["--fix", ...context.braces, "--", ...batch],
        sides,
      );

      const paths = new Set(batch);
      const split: [Split, Split] = [
        splitLines(outcomes[0], paths),
        splitLines(outcomes[1], paths),
      ];

      return compareBatch(where, split, outcomes, false);
    }),
  );

  const written = compare(
    { arrangement, mode: "fix written", batch: arrangement.paths },
    writtenTree(sides.reference, arrangement.paths),
    writtenTree(sides.candidate, arrangement.paths),
  );

  rmSync(copies, { recursive: true, force: true });
  return [...results.flat(), ...written];
}

async function stdinMode(context: Context, arrangement: Arrangement): Promise<Difference[]> {
  const results = await Promise.all(
    arrangement.paths.map(async (path) => {
      const bytes = readFileSync(join(arrangement.dir, path));
      const target = path.startsWith("-") ? `./${path}` : path;
      const where = { arrangement, mode: "stdin", batch: [path] };
      const runs = await Promise.all(
        [
          ["--fix", ...context.braces, "--stdin", target],
          ["--check", "--json", ...context.braces, "--stdin", target],
        ].map((tail) => pair(context, where, tail, sides(arrangement), bytes)),
      );

      const side = (index: 0 | 1) =>
        new Map([[path, runs.flatMap((run) => outcomeLines(run[index]))]]);

      return compare(where, side(0), side(1));
    }),
  );

  return results.flat();
}

function explainLines(context: Context, path: string, findings: Finding[]): number[] {
  const all = new Set(findings.map((finding) => finding.line));
  const ruled = [
    ...new Set(
      findings
        .filter((finding) => !context.rules || context.rules.has(finding.rule))
        .map((finding) => finding.line),
    ),
  ];

  let quiet = 1;
  while (all.has(quiet)) quiet++;

  if (context.explainAll || ruled.length <= EXPLAIN_SAMPLE)
    return [...ruled, quiet].sort((a, b) => a - b);

  const random = seeded(hash(`${context.seed}:explain:${path}`));
  const picked = [quiet];
  while (picked.length <= EXPLAIN_SAMPLE)
    picked.push(ruled.splice(Math.floor(random() * ruled.length), 1)[0]!);

  return picked.sort((a, b) => a - b);
}

async function explainMode(
  context: Context,
  arrangement: Arrangement,
  findings: Map<string, Finding[]>,
): Promise<Difference[]> {
  const results = await Promise.all(
    arrangement.paths.map(async (path) => {
      const where = { arrangement, mode: "explain", batch: [path] };
      const lines = explainLines(context, path, findings.get(path) ?? []);
      const runs = await Promise.all(
        lines.map((line) =>
          pair(
            context,
            where,
            ["explain", ...context.braces, "--", `${path}:${line}`],
            sides(arrangement),
          ),
        ),
      );

      const side = (index: 0 | 1) =>
        new Map([
          [path, runs.flatMap((run, at) => [`line ${lines[at]}`, ...outcomeLines(run[index])])],
        ]);

      return compare(where, side(0), side(1));
    }),
  );

  return results.flat();
}

async function dumpMode(
  context: Context,
  arrangement: Arrangement,
  name: string,
): Promise<Difference[]> {
  const dev = join(dirname(context.candidate), "stanza-dev");
  const dump = join(root, "scripts", "dev", `${name}.ts`);
  const results = await Promise.all(
    batches(arrangement.paths).map(async (batch) => {
      const where = { arrangement, mode: `dump ${name}`, batch };
      const [reference, candidate] = await Promise.all([
        spawn(
          context,
          ["bun", "--no-env-file", "--config=/dev/null", dump, ...batch],
          arrangement.dir,
        ),
        spawn(context, [dev, name, ...batch], arrangement.candidateDir),
      ]);

      return compare(where, whole(reference), whole(candidate));
    }),
  );

  return results.flat();
}

async function compareArrangement(context: Context, full: Arrangement) {
  const needsFindings = context.cleanOnly || context.modes.includes("explain");
  const findings = needsFindings
    ? await referenceFindings(context, full)
    : new Map<string, Finding[]>();

  const arrangement = context.cleanOnly
    ? { ...full, paths: full.paths.filter((path) => (findings.get(path) ?? []).length === 0) }
    : full;

  const runs: Promise<Difference[]>[] = [];
  for (const mode of context.modes)
    if (mode === "check") runs.push(checkMode(context, arrangement));
    else if (mode === "json") runs.push(jsonMode(context, arrangement));
    else if (mode === "fix") runs.push(fixMode(context, arrangement));
    else if (mode === "stdin") runs.push(stdinMode(context, arrangement));
    else runs.push(explainMode(context, arrangement, findings));

  for (const name of context.dumps) runs.push(dumpMode(context, arrangement, name));

  const differences = (await Promise.all(runs)).flat();
  const untouched = compare(
    { arrangement, mode: "read modes written", batch: arrangement.paths },
    writtenTree(arrangement.dir, []),
    writtenTree(arrangement.candidateDir, []),
  );

  return { files: arrangement.paths.length, differences: [...differences, ...untouched] };
}

function runEntries(settings: Settings): number {
  let failed = 0;
  for (const entry of settings.entries) {
    const pattern = entry.pattern ? ["-t", entry.pattern] : [];
    const result = Bun.spawnSync(["bun", "test", entry.file, ...pattern], {
      cwd: root,
      env: { ...process.env, STANZA_BIN: settings.candidate },
      stdout: "inherit",
      stderr: "inherit",
    });

    if (result.success) continue;

    console.log(`entry failed: ${entry.file}${entry.pattern ? ` -t ${entry.pattern}` : ""}`);
    failed++;
  }

  return failed;
}

function report(difference: Difference): string {
  return `${difference.path}  ${difference.layout}  ${difference.mode}\n  reference: ${difference.reference}\n  candidate: ${difference.candidate}`;
}

async function compareAll(
  context: Context,
): Promise<{ files: number; differences: Difference[]; trees: number }> {
  const trees = prepareTrees(context);

  let files = 0;
  const differences: Difference[] = [];
  for (const tree of trees)
    for (const layout of context.layouts) {
      const result = await compareArrangement(context, arrange(tree, layout, context.scratch));
      files += result.files;
      differences.push(...result.differences);
    }

  differences.push(...context.crashes);
  differences.sort(
    (left, right) => left.path.localeCompare(right.path) || left.mode.localeCompare(right.mode),
  );

  return { files, differences, trees: trees.length };
}

async function main(args: string[]): Promise<number> {
  let settings: Settings;
  try {
    settings = parseSettings(args);
  } catch (error: unknown) {
    const parseError = String((error as { code?: unknown }).code).startsWith("ERR_PARSE_ARGS");
    if (!(error instanceof UsageError) && !parseError) throw error;

    console.error(`parity: ${(error as Error).message}\n\n${usage}`);
    return 2;
  }

  const failedEntries = runEntries(settings);
  if (settings.modes.length === 0 && settings.dumps.length === 0) {
    console.log(`parity: ${failedEntries} failed entries, no modes to compare`);
    return failedEntries > 0 ? 1 : 0;
  }

  const scratch = mkdtempSync(join(realpathSync("/tmp"), "stanza-parity-"));
  const cleanUp = () => rmSync(scratch, { recursive: true, force: true });

  for (const [signal, code] of [
    ["SIGINT", 130],
    ["SIGTERM", 143],
  ] as const)
    process.on(signal, () => {
      cleanUp();
      process.exit(code);
    });

  try {
    const { files, differences, trees } = await compareAll({ ...settings, scratch, crashes: [] });
    if (files === 0) {
      console.log("parity: the inputs selected no files, so nothing was compared");
      return 1;
    }

    for (const difference of differences) console.log(report(difference));

    const compared = [...settings.modes, ...settings.dumps.map((name) => `dump ${name}`)];
    console.log(
      `parity: ${differences.length} differences, ${failedEntries} failed entries, ${files} files in ${trees} trees, modes ${compared.join(", ")}, ${settings.layouts.join(" and ")}`,
    );

    return differences.length > 0 || failedEntries > 0 ? 1 : 0;
  } finally {
    cleanUp();
  }
}

if (import.meta.main) process.exit(await main(process.argv.slice(2)));
