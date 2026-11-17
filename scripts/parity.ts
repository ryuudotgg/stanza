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

const root = join(import.meta.dir, "..");

const MODES = ["check", "json", "fix", "stdin", "explain", "hunks"] as const;
type Mode = (typeof MODES)[number];

type Layout = "isolated" | "in-place" | "directory";

const CORPORA = ["Kingshot", "Stanza", "meisai", "F&S", "caasx"].map((name) =>
  join(homedir(), "Projects", name),
);

const BATCH = 100;
const SPAWN_TIMEOUT_MS = 120_000;
const EXPLAIN_SAMPLE = 5;
const BATCH_KEY = "";
const CLEAN_EXITS = new Set(["exit 0", "exit 1", "exit 2"]);

class UsageError extends Error {}

interface Settings {
  reference: string;
  candidate: string;
  inputs: string[];
  modes: Mode[];
  braces: string[];
  layout: Layout;
  mutate: boolean;
  git: boolean;
  checkouts: boolean;
  seed: number;
  explainAll: boolean;
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

const usage = `Usage: bun scripts/parity.ts --reference <binary> --candidate <binary> [options] [paths...]

  --reference <binary>   the binary the candidate is held to, such as a build of origin/main
  --candidate <binary>   the binary compared against the reference, with stanza-dev beside it
  --modes <list>         comma separated: ${MODES.join(", ")}; default all
  --isolate              copy each file alone into scratch, so neither side reads config
  --directory            pass each tree's root directory instead of its files in check, json and fix
  --braces               pass --braces to both sides in check, json, fix, hunks and stdin
  --no-braces            pass --no-braces to both sides in every mode
  --corpora              add ${CORPORA.map((path) => basename(path)).join(", ")} under ~/Projects
  --mutate               add a seeded mutated copy of every input
  --no-git               leave the trees outside git, so both sides take the fallback walk
  --checkouts            copy each input whole, its git directory, untracked and ignored files included,
                         instead of exporting HEAD
  --seed <n>             the mutation seed, default 1
  --explain-all          explain every finding line, not five per file

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

function parseSettings(args: string[]): Settings {
  const { values, positionals } = parseArgs({
    args,
    allowPositionals: true,
    options: {
      candidate: { type: "string" },
      reference: { type: "string" },
      modes: { type: "string" },
      isolate: { type: "boolean", default: false },
      directory: { type: "boolean", default: false },
      braces: { type: "boolean", default: false },
      "no-braces": { type: "boolean", default: false },
      corpora: { type: "boolean", default: false },
      mutate: { type: "boolean", default: false },
      "no-git": { type: "boolean", default: false },
      checkouts: { type: "boolean", default: false },
      seed: { type: "string", default: "1" },
      "explain-all": { type: "boolean", default: false },
    },
  });

  if (!values.reference) throw new UsageError("--reference is required");
  if (!values.candidate) throw new UsageError("--candidate is required");

  const reference = resolve(values.reference);
  if (!existsSync(reference)) throw new UsageError(`no such reference: ${values.reference}`);

  const candidate = resolve(values.candidate);
  if (!existsSync(candidate)) throw new UsageError(`no such candidate: ${values.candidate}`);
  if (!existsSync(join(dirname(candidate), "stanza-dev")))
    throw new UsageError(`no stanza-dev beside ${values.candidate}`);

  const seed = Number(values.seed);
  if (!Number.isSafeInteger(seed)) throw new UsageError(`--seed takes an integer: ${values.seed}`);

  if (values.braces && values["no-braces"])
    throw new UsageError("use one of --braces or --no-braces");

  if (values.isolate && values.directory)
    throw new UsageError("use one of --isolate or --directory");

  if (values.checkouts && values.mutate)
    throw new UsageError("--checkouts commits nothing, so it takes no --mutate");

  if (values.checkouts && values["no-git"])
    throw new UsageError("--checkouts keeps each input's git directory, so it takes no --no-git");

  const layout: Layout = values.isolate ? "isolated" : values.directory ? "directory" : "in-place";
  const diffsInPlace = layout === "in-place" && !values["no-git"] && !values.checkouts;
  const everyMode = MODES.filter((mode) => mode !== "hunks" || diffsInPlace);

  const modes = oneOf(MODES, list(values.modes) ?? everyMode, "mode");
  if (modes.includes("hunks") && !diffsInPlace)
    throw new UsageError(
      "hunks commits each tree and diffs it in place, so it needs the in-place layout and git, and takes no --checkouts",
    );

  const inputs = [...positionals, ...(values.corpora ? CORPORA : [])];
  if (inputs.length === 0) inputs.push(join(root, "tests", "fixtures"));

  for (const input of inputs)
    if (!existsSync(input) || !statSync(input).isDirectory())
      throw new UsageError(`no such directory: ${input}`);

  for (const input of values.checkouts ? inputs : []) {
    const top = toplevel(input);
    if (top !== undefined && top !== realpathSync(input))
      throw new UsageError(
        `--checkouts copies a repository's root, not a directory inside it: ${input}`,
      );
  }

  return {
    reference,
    candidate,
    inputs: inputs.map((input) => resolve(input)),
    modes,
    braces: [values.braces && "--braces", values["no-braces"] && "--no-braces"].filter(
      (flag) => typeof flag === "string",
    ),
    layout,
    mutate: values.mutate,
    git: !values["no-git"],
    checkouts: values.checkouts,
    seed,
    explainAll: values["explain-all"],
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

const rowTail = /^(\d+):(\d+)(?:: ([a-z-]+): (.*?)(?: \(by hand\))?| (\S+) (.*))$/;
const escapes: Record<string, string> = {
  a: "\x07",
  b: "\b",
  t: "\t",
  n: "\n",
  v: "\v",
  f: "\f",
  r: "\r",
};

interface Rows {
  stream: "stdout" | "stderr";
  paths: ReadonlySet<string>;
}

function unquoted(head: string): string {
  if (head.length < 2 || !head.startsWith('"') || !head.endsWith('"')) return head;

  return head
    .slice(1, -1)
    .replace(/\\([0-7]{3}|.)/g, (_, code: string) =>
      code.length === 3 ? String.fromCharCode(Number.parseInt(code, 8)) : (escapes[code] ?? code),
    );
}

function canonicalRow(line: string, paths: ReadonlySet<string>): string {
  let first: string | undefined;
  let known: string | undefined;
  for (let at = line.indexOf(":"); at !== -1; at = line.indexOf(":", at + 1)) {
    const match = rowTail.exec(line.slice(at + 1));
    if (!match) continue;

    const head = line.slice(0, at);
    const path = paths.has(head) ? head : unquoted(head);

    const [, row, col, rule = match[5], message = match[6]] = match;
    const canonical = `${path}:${row}:${col} ${rule} ${message}`;

    first ??= canonical;
    if (paths.has(path)) known = canonical;
  }

  return known ?? first ?? line;
}

function canonicalRows(text: string, paths: ReadonlySet<string>): string {
  return text
    .split("\n")
    .map((line) => canonicalRow(line, paths))
    .join("\n");
}

function rowStream(tail: string[]): Rows["stream"] | undefined {
  if (tail[0] === "explain" || tail.includes("--json")) return undefined;
  return tail.includes("--fix") && tail.includes("--stdin") ? "stderr" : "stdout";
}

function spawn(
  context: Context,
  argv: string[],
  cwd: string,
  stdin?: Uint8Array,
  rows?: Rows,
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

    const shown = (bytes: Uint8Array, stream: Rows["stream"]) => {
      const text = withoutCwd(bytes, cwd);
      return rows?.stream === stream ? canonicalRows(text, rows.paths) : text;
    };

    const [out, err] = [shown(stdout, "stdout"), shown(stderr, "stderr")];
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
  const stream = rowStream(tail);
  const paths = new Set(
    where.arrangement.paths.map((path) => Buffer.from(path).toString("latin1")),
  );

  const rows = stream && { stream, paths };
  const [reference, candidate] = await Promise.all([
    spawn(context, [context.reference, ...tail], dirs.reference, stdin, rows),
    spawn(context, [context.candidate, ...tail], dirs.candidate, stdin, rows),
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

function copyTree(from: string, to: string): void {
  rmSync(to, { recursive: true, force: true });

  if (process.platform !== "darwin") {
    cpSync(from, to, { recursive: true, verbatimSymlinks: true });
    return;
  }

  // A checkout can hold gigabytes of ignored dependencies, and an APFS clone copies them in moments.
  must(["cp", "-cRP", from, to]);
}

function must(argv: string[], cwd?: string): void {
  const result = Bun.spawnSync(argv, { cwd, env, stderr: "pipe" });
  if (!result.success) throw new Error(`${argv.join(" ")} failed: ${result.stderr.toString()}`);
}

function toplevel(dir: string): string | undefined {
  const result = Bun.spawnSync(["git", "-C", dir, "rev-parse", "--show-toplevel"], {
    env,
  });

  return result.success ? realpathSync(result.stdout.toString().trim()) : undefined;
}

function exportInput(input: string, into: string): void {
  mkdirSync(into, { recursive: true });

  if (toplevel(input) !== realpathSync(input)) {
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

function select(context: Context, dir: string): string[] {
  const dev = join(dirname(context.candidate), "stanza-dev");
  const result = Bun.spawnSync([dev, "select", "--keep-generated", "--", dir], { cwd: dir, env });
  if (result.exitCode !== 0) throw new Error(`stanza-dev select failed: ${result.stderr}`);

  const selection = JSON.parse(result.stdout.toString()) as { files: string[]; errors: string[] };
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
    if (context.checkouts) {
      mkdirSync(dirname(dir), { recursive: true });
      copyTree(input, dir);
    } else exportInput(input, dir);

    if (context.git && !context.checkouts) snapshotRepository(dir);

    const files = select(context, dir);
    trees.push({ label, root: dir, files });
    if (!context.mutate) continue;

    const mutated = join(context.scratch, "trees", `${label}~mutated`);
    copyTree(dir, mutated);

    for (const file of files) {
      const path = join(mutated, file);
      const random = seeded(hash(`${context.seed}:${file}`));
      writeFileSync(path, mutate(readFileSync(path, "latin1"), random), "latin1");
    }

    if (context.git) snapshotRepository(mutated);
    trees.push({ label: `${label}~mutated`, root: mutated, files });
  }

  return trees;
}

function withCandidateCopy(arrangement: Omit<Arrangement, "candidateDir">): Arrangement {
  const candidateDir = `${arrangement.dir}~candidate`;
  copyTree(arrangement.dir, candidateDir);
  return { ...arrangement, candidateDir };
}

function neutralName(file: string): string {
  return `source${basename(file).match(/(?:\.d)?\.[^.]+$/)?.[0] ?? ""}`;
}

function arrange(tree: Tree, layout: Layout, scratch: string): Arrangement {
  if (layout !== "isolated") {
    const shown = new Map(tree.files.map((file) => [file, `${tree.label}/${file}`]));
    shown.set(".", `${tree.label}/`);
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

function targets(arrangement: Arrangement): string[][] {
  return arrangement.layout === "directory" ? [["."]] : batches(arrangement.paths);
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

function outcomeLines(outcome: Outcome): string[] {
  return [...outcome.stdout.split("\n"), ...ending(outcome)];
}

function whole(outcome: Outcome): Split {
  return new Map([[BATCH_KEY, outcomeLines(outcome)]]);
}

const findingLine = /^(.*?):(\d+):(\d+) (\S+) /;
function splitLines(outcome: Outcome, paths: ReadonlySet<string>): Split {
  const split: Split = new Map();
  const add = (key: string, line: string) => {
    const lines = split.get(key);
    if (lines) lines.push(line);
    else split.set(key, [line]);
  };

  for (const line of outcome.stdout.split("\n")) {
    const match = line.match(findingLine);
    add(match && paths.has(match[1]!) ? match[1]! : BATCH_KEY, line);
  }

  for (const line of ending(outcome)) add(BATCH_KEY, line);
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

function splitJson(outcome: Outcome): Split {
  const findings = parseFindings(outcome.stdout);
  if (!findings) return whole(outcome);

  const split: Split = new Map();
  for (const finding of findings) {
    const lines = split.get(finding.path) ?? [];
    lines.push(JSON.stringify(finding));
    split.set(finding.path, lines);
  }

  split.set(BATCH_KEY, ending(outcome));
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

function compareBatch(where: Where, split: [Split, Split], outcomes: [Outcome, Outcome]) {
  const differences = compare(where, ...split);
  if (differences.length > 0) return differences;
  return compare(where, whole(outcomes[0]), whole(outcomes[1]));
}

async function referenceFindings(context: Context, arrangement: Arrangement) {
  const found = new Map<string, Finding[]>(arrangement.paths.map((path) => [path, []]));
  const outcomes = await Promise.all(
    batches(arrangement.paths).map((batch) =>
      spawn(
        context,
        [context.reference, "--check", "--json", ...context.braces, "--", ...batch],
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
    targets(arrangement).map(async (batch) => {
      const where = { arrangement, mode: "check", batch };
      const outcomes = await pair(
        context,
        where,
        ["--check", ...context.braces, "--", ...batch],
        sides(arrangement),
      );

      const paths = new Set(arrangement.layout === "directory" ? arrangement.paths : batch);
      const split: [Split, Split] = [
        splitLines(outcomes[0], paths),
        splitLines(outcomes[1], paths),
      ];

      return compareBatch(where, split, outcomes);
    }),
  );

  return results.flat();
}

async function jsonMode(context: Context, arrangement: Arrangement): Promise<Difference[]> {
  const results = await Promise.all(
    targets(arrangement).map(async (batch) => {
      const where = { arrangement, mode: "json", batch };
      const tail = ["--check", "--json", ...context.braces, "--", ...batch];
      const outcomes = await pair(context, where, tail, sides(arrangement));
      return compareBatch(where, [splitJson(outcomes[0]), splitJson(outcomes[1])], outcomes);
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
  copyTree(arrangement.dir, sides.reference);
  copyTree(arrangement.dir, sides.candidate);

  const results = await Promise.all(
    targets(arrangement).map(async (batch) => {
      const where = { arrangement, mode: "fix", batch };
      const outcomes = await pair(
        context,
        where,
        ["--fix", ...context.braces, "--", ...batch],
        sides,
      );

      const paths = new Set(arrangement.layout === "directory" ? arrangement.paths : batch);
      const split: [Split, Split] = [
        splitLines(outcomes[0], paths),
        splitLines(outcomes[1], paths),
      ];

      return compareBatch(where, split, outcomes);
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

async function hunksMode(context: Context, arrangement: Arrangement): Promise<Difference[]> {
  if (arrangement.layout !== "in-place") return [];

  const copies = mkdtempSync(join(context.scratch, "hunks-"));
  const sides = { reference: join(copies, "reference"), candidate: join(copies, "candidate") };
  for (const dir of [sides.reference, sides.candidate]) {
    copyTree(arrangement.dir, dir);

    for (const file of arrangement.paths) {
      const path = join(dir, file);
      const random = seeded(hash(`${context.seed}:${file}`));
      writeFileSync(path, mutate(readFileSync(path, "latin1"), random), "latin1");
    }
  }

  const paths = new Set(arrangement.paths);
  const differences: Difference[] = [];
  for (const mode of ["--check", "--fix"]) {
    const where = { arrangement, mode: `hunks ${mode.slice(2)}`, batch: arrangement.paths };
    const tail = [mode, "--changed", "--hunks", ...context.braces];
    const outcomes = await pair(context, where, tail, sides);
    const split: [Split, Split] = [splitLines(outcomes[0], paths), splitLines(outcomes[1], paths)];
    differences.push(...compareBatch(where, split, outcomes));
  }

  const where = { arrangement, mode: "hunks", batch: arrangement.paths };
  const written = compare(
    { ...where, mode: "hunks written" },
    writtenTree(sides.reference, arrangement.paths),
    writtenTree(sides.candidate, arrangement.paths),
  );

  rmSync(copies, { recursive: true, force: true });
  return [...differences, ...written];
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
  const lines = [...all];

  let quiet = 1;
  while (all.has(quiet)) quiet++;

  if (context.explainAll || lines.length <= EXPLAIN_SAMPLE)
    return [...lines, quiet].sort((a, b) => a - b);

  const random = seeded(hash(`${context.seed}:explain:${path}`));
  const picked = [quiet];
  while (picked.length <= EXPLAIN_SAMPLE)
    picked.push(lines.splice(Math.floor(random() * lines.length), 1)[0]!);

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
            [
              "explain",
              ...context.braces.filter((flag) => flag !== "--braces"),
              "--",
              `${path}:${line}`,
            ],
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

async function compareArrangement(context: Context, arrangement: Arrangement) {
  const { modes } = context;
  const findings = modes.includes("explain")
    ? await referenceFindings(context, arrangement)
    : new Map<string, Finding[]>();

  const runs: Promise<Difference[]>[] = [];
  for (const mode of modes)
    if (mode === "check") runs.push(checkMode(context, arrangement));
    else if (mode === "json") runs.push(jsonMode(context, arrangement));
    else if (mode === "fix") runs.push(fixMode(context, arrangement));
    else if (mode === "stdin") runs.push(stdinMode(context, arrangement));
    else if (mode === "hunks") runs.push(hunksMode(context, arrangement));
    else runs.push(explainMode(context, arrangement, findings));

  const differences = (await Promise.all(runs)).flat();
  const untouched = compare(
    { arrangement, mode: "read modes written", batch: arrangement.paths },
    writtenTree(arrangement.dir, []),
    writtenTree(arrangement.candidateDir, []),
  );

  return { files: arrangement.paths.length, differences: [...differences, ...untouched] };
}

function report(difference: Difference): string {
  return `${difference.path}  ${difference.layout}  ${difference.mode}\n  reference: ${difference.reference}\n  candidate: ${difference.candidate}`;
}

async function compareAll(
  context: Context,
): Promise<{ files: number; directories: number; differences: Difference[]; trees: number }> {
  const trees = prepareTrees(context);

  let files = 0;
  let directories = 0;
  const differences: Difference[] = [];
  const walksTree = context.modes.some((mode) => mode !== "stdin" && mode !== "explain");
  for (const tree of trees) {
    const result = await compareArrangement(
      context,
      arrange(tree, context.layout, context.scratch),
    );

    files += result.files;
    if (context.layout === "directory" && walksTree) directories++;
    differences.push(...result.differences);
  }

  differences.push(...context.crashes);
  differences.sort(
    (left, right) => left.path.localeCompare(right.path) || left.mode.localeCompare(right.mode),
  );

  return { files, directories, differences, trees: trees.length };
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
    const { files, directories, differences, trees } = await compareAll({
      ...settings,
      scratch,
      crashes: [],
    });

    if (files === 0 && directories === 0) {
      for (const difference of differences) console.log(report(difference));
      console.log("parity: the inputs selected no files, so nothing was compared");
      return 1;
    }

    for (const difference of differences) console.log(report(difference));

    console.log(
      `parity: ${differences.length} differences, ${files} files in ${trees} trees, modes ${settings.modes.join(", ")}, ${settings.layout}`,
    );

    return differences.length > 0 ? 1 : 0;
  } finally {
    cleanUp();
  }
}

if (import.meta.main) process.exit(await main(process.argv.slice(2)));
