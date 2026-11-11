import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { basename, extname, join, relative } from "node:path";

const root = join(import.meta.dir, "..");
const corpus = join(root, "fuzz", "corpus");

const SEED_CAP = 64 * 1024;
const PATTERN_CAP = 1024;
const PATH_CAP = 4096;
const COMMITS = 200;
const PATHS_PER_PATTERN = 4;

const EXTENSIONS = [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"];
const SKIPPED = new Set(["node_modules", ".git", "target", "corpus", "artifacts"]);

const usage = `usage: bun scripts/fuzz-seeds.ts [path...]

Writes fuzz/corpus/<target>/ from every file under the paths, default tests/fixtures and the
repository, and from the git history of each path that is a repository.`;

type Target = "format" | "hook_input" | "git_diff" | "globs";

const counts: Record<Target, number> = { format: 0, hook_input: 0, git_diff: 0, globs: 0 };
function seed(target: Target, bytes: Uint8Array): void {
  if (bytes.length === 0 || bytes.length > SEED_CAP) return;

  const directory = join(corpus, target);
  mkdirSync(directory, { recursive: true });
  writeFileSync(join(directory, createHash("sha1").update(bytes).digest("hex")), bytes);
  counts[target]++;
}

function files(directory: string): string[] {
  const found: string[] = [];
  const pending = [directory];
  while (pending.length > 0) {
    const current = pending.pop()!;
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      if (SKIPPED.has(entry.name)) continue;

      const path = join(current, entry.name);
      if (entry.isDirectory()) pending.push(path);
      else if (entry.isFile()) found.push(path);
    }
  }

  return found.sort();
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

function seedPath(input: string): void {
  const encoder = new TextEncoder();
  const all = statSync(input).isDirectory() ? files(input) : [input];
  const names = all
    .map((path) => relative(input, path))
    .filter((name) => Buffer.byteLength(name) <= PATH_CAP);

  for (const [index, path] of all.entries()) {
    const extension = extname(path);
    const selector = EXTENSIONS.indexOf(extension);
    const text = readFileSync(path);
    if (selector >= 0) seed("format", new Uint8Array([selector, ...text]));
    if (extension === ".json") seed("hook_input", text);
    if (extension === ".jsonl")
      for (const line of text.toString().split("\n")) seed("hook_input", encoder.encode(line));

    for (const pattern of patterns(path, text.toString())) {
      if (Buffer.byteLength(pattern) > PATTERN_CAP) continue;

      for (let offset = 0; offset < PATHS_PER_PATTERN && names.length > 0; offset++) {
        const name = names[(index + offset * 7919) % names.length]!;
        seed("globs", encoder.encode(`${pattern}\0${name}`));
      }
    }
  }

  if (existsSync(join(input, ".git")))
    for (const commit of history(input)) seed("git_diff", encoder.encode(commit));
}

function main(args: string[]): number {
  if (args.includes("--help") || args.includes("-h")) {
    console.log(usage);
    return 0;
  }

  for (const target of Object.keys(counts)) mkdirSync(join(corpus, target), { recursive: true });

  const inputs = args.length > 0 ? args : [join(root, "tests", "fixtures"), root];
  for (const input of inputs) seedPath(input);

  for (const [target, count] of Object.entries(counts)) console.log(`${target}: ${count} seeds`);
  return 0;
}

process.exit(main(process.argv.slice(2)));
