import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  realpathSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { basename, extname, isAbsolute, join, relative, sep } from "node:path";
import { parseArgs } from "node:util";

const root = join(import.meta.dir, "..");
const corpus = join(root, "fuzz", "corpus");
const maintained = join(root, "fuzz", "seeds");

const SEED_CAP = 64 * 1024;
const PATTERN_CAP = 1024;
const PATH_CAP = 4096;
const COMMITS = 200;
const PATHS_PER_PATTERN = 4;

const TARGETS = ["format", "hook_input", "git_diff", "globs"] as const;
const EXTENSIONS = [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"];
const PATTERN_FILES = [".gitignore", ".editorconfig"];
const SKIPPED = new Set(["node_modules", ".git", "target", "corpus", "artifacts"]);

const usage = `usage: bun scripts/fuzz-seeds.ts [--target <${TARGETS.join("|")}>] [path...]

Writes new seeds into fuzz/corpus/<target>/ from fuzz/seeds/ and every file under the paths,
default the repository, and from the git history of each path that is a repository.
--target seeds that target alone.`;

type Target = (typeof TARGETS)[number];

const counts = new Map<Target, number>();
function seed(target: Target, bytes: Uint8Array): void {
  if (!counts.has(target) || bytes.length === 0 || bytes.length > SEED_CAP) return;

  const path = join(corpus, target, createHash("sha1").update(bytes).digest("hex"));
  if (existsSync(path)) return;

  writeFileSync(path, bytes);
  counts.set(target, counts.get(target)! + 1);
}

function files(directory: string): string[] {
  const found: string[] = [];
  const pending = [directory];
  while (pending.length > 0) {
    const current = pending.pop()!;
    const entries = readdirSync(current, { withFileTypes: true });
    for (const entry of entries.filter((entry) => !SKIPPED.has(entry.name))) {
      const path = join(current, entry.name);
      if (entry.isDirectory()) pending.push(path);
      else if (entry.isFile()) found.push(path);
    }
  }

  return found.sort();
}

function wanted(path: string): boolean {
  const extension = extname(path);
  if (counts.has("globs") && PATTERN_FILES.includes(basename(path))) return true;
  if (counts.has("hook_input") && extension === ".jsonl") return true;

  const format = counts.has("format") && EXTENSIONS.includes(extension);
  const json = counts.has("hook_input") && extension === ".json";
  if (!format && !json) return false;

  return statSync(path).size <= (format ? SEED_CAP - 1 : SEED_CAP);
}

function patterns(path: string, text: string): string[] {
  if (basename(path) === ".gitignore")
    return text.split("\n").filter((line) => line.trim() !== "" && !line.startsWith("#"));

  if (basename(path) === ".editorconfig")
    return [...text.matchAll(/^\s*\[(.*)\]\s*$/gm)].map((match) => match[1]!);

  return [];
}

function history(directory: string): string[] {
  const log = Bun.spawnSync(
    [
      "git",
      "-C",
      directory,
      "log",
      "-p",
      "--unified=999999999",
      "--text",
      "--submodule=short",
      "--no-color",
      "--no-ext-diff",
      "--no-textconv",
      "--no-renames",
      "--format=%x00",
      `-n${COMMITS}`,
    ],
    { stdout: "pipe", stderr: "ignore" },
  );

  return log.success ? log.stdout.toString().split("\0") : [];
}

function sections(commit: string): string[] {
  if (Buffer.byteLength(commit) <= SEED_CAP) return [commit];
  return commit.split(/(?=^diff --git )/m).filter((section) => section.startsWith("diff --git "));
}

function seedPath(input: string): void {
  const encoder = new TextEncoder();
  const all = statSync(input).isDirectory() ? files(input) : [input];
  const names = all
    .map((path) => relative(input, path))
    .filter((name) => Buffer.byteLength(name) <= PATH_CAP);

  for (const [index, path] of all.entries()) {
    if (!wanted(path)) continue;

    const extension = extname(path);
    const selector = EXTENSIONS.indexOf(extension);
    const text = readFileSync(path);
    if (selector >= 0) {
      const bytes = new Uint8Array(text.length + 1);
      bytes[0] = selector;
      bytes.set(text, 1);
      seed("format", bytes);
    }

    if (extension === ".json") seed("hook_input", text);

    if (extension === ".jsonl") {
      seed("hook_input", text);
      for (const line of text.toString().split("\n")) seed("hook_input", encoder.encode(line));
    }

    for (const pattern of patterns(path, text.toString())) {
      if (Buffer.byteLength(pattern) > PATTERN_CAP) continue;

      for (let offset = 0; offset < PATHS_PER_PATTERN && names.length > 0; offset++) {
        const name = names[(index + offset * 7919) % names.length]!;
        seed("globs", encoder.encode(`${pattern}\0${name}`));
      }
    }
  }

  if (counts.has("git_diff") && existsSync(join(input, ".git")))
    for (const commit of history(input))
      for (const section of sections(commit)) seed("git_diff", encoder.encode(section));
}

function within(directory: string, path: string): boolean {
  const rest = relative(realpathSync(directory), realpathSync(path));
  if (rest.startsWith("..") || isAbsolute(rest)) return false;
  return !rest.split(sep).some((name) => SKIPPED.has(name));
}

function main(argv: string[]): number {
  let parsed;
  try {
    parsed = parseArgs({
      args: argv,
      options: { target: { type: "string" }, help: { type: "boolean", short: "h" } },
      allowPositionals: true,
    });
  } catch (error) {
    console.error(`${(error as Error).message}\n\n${usage}`);
    return 2;
  }

  const { values, positionals } = parsed;
  if (values.help) {
    console.log(usage);
    return 0;
  }

  const target = values.target;
  if (target !== undefined && !TARGETS.includes(target as Target)) {
    console.error(`unknown target ${target}\n\n${usage}`);
    return 2;
  }

  for (const selected of target === undefined ? TARGETS : [target as Target]) {
    mkdirSync(join(corpus, selected), { recursive: true });
    counts.set(selected, 0);
  }

  const inputs = positionals.length > 0 ? positionals : [root];
  if (!inputs.some((input) => within(input, maintained))) inputs.push(maintained);
  for (const input of inputs) seedPath(input);

  for (const [name, count] of counts)
    console.log(`${name}: ${count} new seed${count === 1 ? "" : "s"}`);

  return 0;
}

process.exit(main(process.argv.slice(2)));
