import { existsSync, lstatSync, readdirSync, readFileSync, realpathSync, statSync } from "node:fs";
import {
  basename,
  delimiter,
  dirname,
  extname,
  isAbsolute,
  join,
  relative,
  resolve,
  sep,
} from "node:path";
import { ignoredByRules, parseIgnore, type Rule } from "./gitignore.ts";
import { diffChanges } from "./hunks.ts";
import type { Changed } from "./model.ts";

export interface Collected {
  files: string[];
  errors: string[];
  warnings: string[];
}

export const extensions = new Set([".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"]);
const skippedSegments = new Set([
  "node_modules",
  "dist",
  "build",
  ".git",
  ".next",
  "out",
  "coverage",
  "migrations",
  "drizzle",
]);

let gitAvailable: boolean | undefined;

function hasGit(): boolean {
  return (gitAvailable ??= Bun.which("git") !== null);
}

type GitResult = { ok: true; output: string } | { ok: false; error: string };

type GitBytes = { ok: true; output: Uint8Array } | { ok: false; error: string };

type Selection = { ok: true; files: string[] } | { ok: false; error: string };

function runGit(
  cwd: string,
  args: string[],
  stdin?: Uint8Array,
  env?: NodeJS.ProcessEnv,
): GitResult {
  const result = runGitBytes(cwd, args, stdin, env);
  return result.ok ? { ok: true, output: new TextDecoder().decode(result.output) } : result;
}

function runGitBytes(
  cwd: string,
  args: string[],
  stdin?: Uint8Array,
  env?: NodeJS.ProcessEnv,
): GitBytes {
  const result = Bun.spawnSync(["git", "-C", cwd, ...args], {
    stdin,
    stderr: "pipe",
    // Bun snapshots the environment for a spawn given no env, so a later process.env change would miss git.
    env: env ?? process.env,
  });

  if (result.exitCode === 0) return { ok: true, output: result.stdout };

  const reason =
    new TextDecoder()
      .decode(result.stderr)
      .split("\n")
      .map((line) => line.trim())
      .find(Boolean) ?? `exit ${result.exitCode}`;

  const command = args.find((arg) => !arg.startsWith("-"));
  return { ok: false, error: `git ${command} failed in ${cwd}: ${reason}` };
}

function nulItems(text: string): string[] {
  return text.split("\0").filter(Boolean);
}

function supported(path: string): boolean {
  return extensions.has(extname(path));
}

function generatedName(name: string): boolean {
  return (
    name.endsWith(".gen.ts") ||
    name.endsWith(".gen.tsx") ||
    name.includes(".generated.") ||
    name.endsWith(".min.js")
  );
}

function skippedName(path: string, keepGenerated: boolean): boolean {
  const name = basename(path);
  return (
    name.endsWith(".d.ts") ||
    name.endsWith(".d.mts") ||
    name.endsWith(".d.cts") ||
    (!keepGenerated && generatedName(name))
  );
}

function hasSkippedSegment(path: string): boolean {
  return path.split(/[\\/]+/).some((segment) => skippedSegments.has(segment));
}

export function isCandidate(relativePath: string, keepGenerated = false): boolean {
  return (
    supported(relativePath) &&
    !skippedName(relativePath, keepGenerated) &&
    !hasSkippedSegment(relativePath)
  );
}

export function isGeneratedHeader(text: string): boolean {
  return text
    .split(/\r?\n/, 10)
    .some(
      (line) => line.includes("@generated") || /DO NOT EDIT|automatically generated/i.test(line),
    );
}

export function errorCode(error: unknown): string {
  const code = (error as NodeJS.ErrnoException).code;
  return typeof code === "string" ? code : String(error);
}

interface IgnoreSource {
  directory: string;
  rules: Rule[];
}

function ignoredBySources(sources: IgnoreSource[], path: string, isDirectory: boolean): boolean {
  let ignored = false;
  for (const source of sources) {
    const relativePath = relative(source.directory, path).split(sep).join("/");
    ignored = ignoredByRules(source.rules, relativePath, isDirectory) ?? ignored;
  }

  return ignored;
}

function fallbackFiles(
  dir: string,
  cwd: string,
  keepGenerated: boolean,
): { files: string[]; errors: string[] } {
  const files: string[] = [];
  const errors: string[] = [];
  function withIgnoreFile(current: string, sources: IgnoreSource[]): IgnoreSource[] {
    const ignoreFile = join(current, ".gitignore");
    if (!existsSync(ignoreFile)) return sources;

    try {
      return [
        ...sources,
        { directory: current, rules: parseIgnore(readFileSync(ignoreFile, "utf8")) },
      ];
    } catch (error: unknown) {
      errors.push(`cannot read ${relative(cwd, ignoreFile)}: ${errorCode(error)}`);
      return sources;
    }
  }

  function walk(current: string, parents: IgnoreSource[]): void {
    const active = withIgnoreFile(current, parents);

    let entries;
    try {
      entries = readdirSync(current, { withFileTypes: true });
    } catch (error: unknown) {
      errors.push(
        `cannot read directory: ${relative(cwd, current) || current}: ${errorCode(error)}`,
      );

      return;
    }

    for (const entry of entries) {
      const file = join(current, entry.name);
      const rel = relative(dir, file).split(sep).join("/");
      if (hasSkippedSegment(rel)) continue;
      if (ignoredBySources(active, file, entry.isDirectory())) continue;

      if (entry.isDirectory()) walk(file, active);
      else if (entry.isFile() && isCandidate(rel, keepGenerated)) files.push(resolve(file));
    }
  }

  const ancestors: string[] = [];
  for (let current = dir; !existsSync(join(current, ".git")) && dirname(current) !== current;) {
    current = dirname(current);
    ancestors.unshift(current);
  }

  let inherited: IgnoreSource[] = [];
  for (const [index, ancestor] of ancestors.entries()) {
    inherited = withIgnoreFile(ancestor, inherited);
    const below = ancestors[index + 1] ?? dir;
    if (ignoredBySources(inherited, below, true)) return { files, errors };
  }

  walk(dir, inherited);
  return { files, errors };
}

export type Location =
  | { kind: "repository"; root: string }
  | { kind: "outside" }
  | { kind: "failed"; error: string };

function ceilingDirectories(): Set<string> {
  return new Set(
    (process.env.GIT_CEILING_DIRECTORIES ?? "")
      .split(delimiter)
      .filter((path) => isAbsolute(path))
      .map((path) => (existsSync(path) ? realpathSync(path) : resolve(path))),
  );
}

function hasGitMarker(dir: string): boolean {
  if (dir.split(sep).includes(".git")) return false;

  const ceilings = ceilingDirectories();
  for (let current = realpathSync(dir); ; current = dirname(current)) {
    if (existsSync(join(current, ".git"))) return true;
    const parent = dirname(current);
    if (parent === current || ceilings.has(parent)) return false;
  }
}

export function locate(dir: string): Location {
  if (!hasGit()) return { kind: "outside" };

  const result = runGit(dir, ["rev-parse", "--show-toplevel"]);
  const root = result.ok ? result.output.trim() : "";
  if (root) return { kind: "repository", root };
  if (!result.ok && hasGitMarker(dir)) return { kind: "failed", error: result.error };
  return { kind: "outside" };
}

function repositoryPath(root: string, file: string): string {
  return relative(root, file).split(sep).join("/");
}

function within(root: string, path: string): boolean {
  const pathFromRoot = relative(root, path);
  return pathFromRoot !== ".." && !pathFromRoot.startsWith(`..${sep}`) && !isAbsolute(pathFromRoot);
}

function suppliedBelow(anchor: string, path: string): string {
  for (let current = dirname(path); ; current = dirname(current)) {
    if (existsSync(current) && realpathSync(current) === anchor)
      return repositoryPath(current, path);
    if (dirname(current) === current) return basename(path);
  }
}

function selectedBelow(
  anchor: string,
  supplied: string,
  file: string,
  keepGenerated = false,
): boolean {
  return (
    isCandidate(repositoryPath(anchor, file), keepGenerated) &&
    isCandidate(suppliedBelow(anchor, supplied), keepGenerated)
  );
}

function isSet(value: string | undefined): boolean {
  return value === "true" || value === "set";
}

type Attributes = { ok: true; values: Map<string, string> } | { ok: false; error: string };

function readAttributes(
  files: string[],
  root: string,
  names: string[],
  source: "worktree" | "index",
): Attributes {
  if (files.length === 0) return { ok: true, values: new Map() };

  const paths = files.map((file) => `${repositoryPath(root, file)}\0`).join("");
  const result = runGit(
    root,
    ["check-attr", ...(source === "index" ? ["--cached"] : []), ...names, "-z", "--stdin"],
    new TextEncoder().encode(paths),
  );

  if (!result.ok) return result;

  const values = new Map<string, string>();
  const fields = result.output.split("\0");
  for (let index = 0; index + 2 < fields.length; index += 3)
    values.set(`${fields[index]}\0${fields[index + 1]}`, fields[index + 2]!);

  return { ok: true, values };
}

function attribute(
  attributes: Map<string, string>,
  root: string,
  file: string,
  name: string,
): string | undefined {
  return attributes.get(`${repositoryPath(root, file)}\0${name}`);
}

function dropGeneratedAttributes(files: string[], root: string): Selection {
  const read = readAttributes(files, root, ["linguist-generated"], "worktree");
  if (!read.ok) return read;

  return {
    ok: true,
    files: files.filter((file) => !isSet(attribute(read.values, root, file, "linguist-generated"))),
  };
}

function directoryFiles(dir: string, root: string, keepGenerated: boolean): Selection {
  const result = runGit(dir, [
    "ls-files",
    "-z",
    "--cached",
    "--others",
    "--exclude-standard",
    "--",
    ".",
  ]);

  if (!result.ok) return result;

  const files = nulItems(result.output)
    .filter((path) => isCandidate(path, keepGenerated))
    .map((path) => resolve(dir, path))
    .filter((path) => existsSync(path) && lstatSync(path).isFile());

  const inside = files.map((path) => fileRealpath(path)).filter((path) => within(root, path));
  return { ok: true, files: inside };
}

function fileRealpath(path: string): string {
  try {
    return realpathSync(path);
  } catch (error: unknown) {
    if (lstatSync(path).isFile()) return join(realpathSync(dirname(path)), basename(path));
    throw error;
  }
}

function sorted(files: string[]): string[] {
  return [...new Set(files)].sort((left, right) => left.localeCompare(right));
}

export function collectFiles(paths: string[], cwd: string, keepGenerated = false): Collected {
  const locations = new Map<string, Location>();
  const byRoot = new Map<string, string[]>();
  const outside: string[] = [];
  const errors: string[] = [];
  const warnings = hasGit()
    ? []
    : ["git not found on PATH, file selection fell back to the directory walk"];

  function locationOf(dir: string): Location {
    const known = locations.get(dir);
    if (known) return known;

    const location = locate(dir);
    locations.set(dir, location);
    return location;
  }

  function add(root: string, files: string[]): void {
    const listed = byRoot.get(root);
    if (listed) listed.push(...files);
    else byRoot.set(root, files);
  }

  for (const input of paths) {
    const path = isAbsolute(input) ? input : resolve(cwd, input);
    if (!existsSync(path)) {
      errors.push(`no such file: ${input}`);
      continue;
    }

    let directory = false;
    try {
      directory = statSync(path).isDirectory();
    } catch {}

    if (directory) {
      const location = locationOf(path);
      if (location.kind === "failed") {
        errors.push(location.error);
        continue;
      }

      if (location.kind === "outside") {
        const fallback = fallbackFiles(path, cwd, keepGenerated);
        outside.push(...fallback.files);
        errors.push(...fallback.errors);
        continue;
      }

      const listed = directoryFiles(path, location.root, keepGenerated);
      if (listed.ok) add(location.root, listed.files);
      else errors.push(listed.error);

      continue;
    }

    if (!supported(path)) {
      errors.push(`not a TypeScript or JavaScript file: ${input}`);
      continue;
    }

    const file = fileRealpath(path);
    const location = locationOf(dirname(file));
    if (location.kind === "failed") {
      errors.push(location.error);
      continue;
    }

    const anchor = location.kind === "repository" ? location.root : dirname(file);
    if (!selectedBelow(anchor, path, file, keepGenerated)) continue;

    if (location.kind === "outside") outside.push(file);
    else add(location.root, [file]);
  }

  const files = [...outside];
  for (const [root, candidates] of byRoot) {
    if (keepGenerated) {
      files.push(...candidates);
      continue;
    }

    const kept = dropGeneratedAttributes(candidates, root);
    if (kept.ok) files.push(...kept.files);
    else errors.push(kept.error);
  }

  return { files: sorted(files), errors: [...new Set(errors)], warnings };
}

function existingAncestor(path: string): string {
  const parent = dirname(path);
  return existsSync(path) || parent === path ? path : existingAncestor(parent);
}

export type StdinTarget =
  | { status: "format"; path: string; root: string | undefined }
  | { status: "skip" }
  | { status: "unsupported"; error: string }
  | { status: "failed"; error: string };

export function stdinTarget(input: string, cwd: string): StdinTarget {
  const path = isAbsolute(input) ? input : resolve(cwd, input);
  if (!supported(path))
    return { status: "unsupported", error: `not a TypeScript or JavaScript file: ${input}` };

  const directory = existingAncestor(dirname(path));
  const real = realpathSync(directory);
  const file = join(real, relative(directory, path));

  const location = locate(real);
  if (location.kind === "failed") return { status: "failed", error: location.error };

  const anchor = location.kind === "repository" ? location.root : dirname(file);
  if (!selectedBelow(anchor, path, file)) return { status: "skip" };
  if (location.kind === "outside") return { status: "format", path: file, root: undefined };

  const kept = dropGeneratedAttributes([file], location.root);
  if (!kept.ok) return { status: "failed", error: kept.error };
  if (kept.files.length === 0) return { status: "skip" };

  return { status: "format", path: file, root: location.root };
}

export function landsWithin(root: string, path: string): boolean {
  try {
    let entry = path;
    while (!lstatSync(entry, { throwIfNoEntry: false }) && dirname(entry) !== entry)
      entry = dirname(entry);
    return within(root, realpathSync(entry));
  } catch {
    return false;
  }
}

export function ignoredByGit(root: string, path: string): boolean {
  return runGit(root, ["check-ignore", "-q", "--", repositoryPath(root, path)]).ok;
}

export function trackedInHead(root: string, path: string): boolean {
  return runGit(root, ["cat-file", "-e", `HEAD:${repositoryPath(root, path)}`]).ok;
}

function diffSections(text: string): string[][] {
  const sections: string[][] = [];
  for (const line of text.split("\n")) {
    if (line.startsWith("diff --git ") && sections.at(-1)?.[0] !== line) sections.push([]);
    sections.at(-1)?.push(line);
  }

  return sections;
}

type ChangedLines = { ok: true; lines: Changed[] } | { ok: false; error: string };

function diffLines(
  root: string,
  range: string[],
  count: number,
  paths: string[] = [],
): ChangedLines {
  const env = { ...process.env };
  delete env.GIT_DIFF_OPTS;
  const diff = runGit(
    root,
    [
      "-c",
      "diff.suppressBlankEmpty=false",
      "--literal-pathspecs",
      "diff",
      "--unified=999999999",
      "--text",
      "--submodule=short",
      "--no-color",
      "--no-ext-diff",
      "--no-textconv",
      "--no-renames",
      ...range,
      "--",
      ...paths,
    ],
    undefined,
    env,
  );

  if (!diff.ok) return diff;

  const sections = diffSections(diff.output);
  if (sections.length !== count)
    return { ok: false, error: "git diff section count differs from the changed file count" };

  return { ok: true, lines: sections.map(diffChanges) };
}

export function collectChanged(
  cwd: string,
  location: Location = locate(cwd),
  hunks = false,
): Collected & { changedLines: Map<string, Changed> } {
  const changedLines = new Map<string, Changed>();
  if (!hasGit())
    return {
      changedLines,
      files: [],
      errors: ["--changed needs git, which was not found on PATH"],
      warnings: [],
    };

  if (location.kind === "failed")
    return { files: [], errors: [location.error], warnings: [], changedLines };
  if (location.kind === "outside")
    return { files: [], errors: ["not inside a git repository"], warnings: [], changedLines };

  const root = location.root;

  const born = runGit(root, ["rev-parse", "--verify", "-q", "HEAD"]).ok;
  if (!born) {
    const branch = runGit(root, ["symbolic-ref", "-q", "HEAD"]);
    if (!branch.ok) return { files: [], errors: [branch.error], warnings: [], changedLines };
  }

  const changed = born
    ? runGit(root, ["diff", "--name-only", "-z", "--no-renames", "--submodule=short", "HEAD", "--"])
    : runGit(root, ["ls-files", "-z", "--cached"]);

  const untracked = runGit(root, ["ls-files", "-z", "--others", "--exclude-standard"]);
  if (!changed.ok || !untracked.ok) {
    const errors = [changed, untracked].flatMap((result) => (result.ok ? [] : [result.error]));
    return { files: [], errors: [...new Set(errors)], warnings: [], changedLines };
  }

  const names = nulItems(changed.output);
  const candidates = names.filter((name) => isCandidate(name));
  if (hunks && born && candidates.length > 0) {
    const diff = diffLines(root, ["HEAD"], candidates.length, candidates);
    if (!diff.ok) return { files: [], errors: [diff.error], warnings: [], changedLines };
    for (let index = 0; index < candidates.length; index++)
      changedLines.set(resolve(root, candidates[index]!), diff.lines[index]!);
  }

  const untrackedNames = nulItems(untracked.output);
  for (const name of untrackedNames) changedLines.delete(resolve(root, name));

  const realRoot = realpathSync(root);
  const files = [...names, ...untrackedNames]
    .filter((path) => isCandidate(path))
    .map((path) => resolve(root, path))
    .filter((path) => existsSync(path) && lstatSync(path).isFile())
    .filter((path) => within(realRoot, fileRealpath(path)));

  const kept = dropGeneratedAttributes(files, root);
  if (!kept.ok) return { files: [], errors: [kept.error], warnings: [], changedLines };

  return { files: sorted(kept.files), errors: [], warnings: [], changedLines };
}

export interface StagedFile {
  path: string;
  bytes: Uint8Array;
  unstaged: boolean;
  lines?: Changed;
}

export type StagedSelection = { ok: true; files: StagedFile[] } | { ok: false; error: string };

const regularFileModes = new Set(["100644", "100755"]);

type Blobs = { ok: true; blobs: Uint8Array[] } | { ok: false; error: string };

function readBlobs(root: string, ids: string[]): Blobs {
  if (ids.length === 0) return { ok: true, blobs: [] };

  const result = runGitBytes(
    root,
    ["cat-file", "--batch"],
    new TextEncoder().encode(`${ids.join("\n")}\n`),
  );

  if (!result.ok) return result;

  const output = result.output;
  const blobs: Uint8Array[] = [];

  let offset = 0;
  for (const id of ids) {
    const end = output.indexOf(10, offset);
    const [, type, size] =
      end < 0 ? [] : new TextDecoder().decode(output.subarray(offset, end)).split(" ");

    if (type !== "blob") return { ok: false, error: `git cat-file could not read blob ${id}` };

    const start = end + 1;
    blobs.push(output.subarray(start, start + Number(size)));
    offset = start + Number(size) + 1;
  }

  return { ok: true, blobs };
}

function readFilteredBlob(
  root: string,
  tree: string | undefined,
  path: string,
  id: string,
): GitBytes {
  return runGitBytes(root, [
    ...(tree === undefined ? [] : [`--attr-source=${tree}`]),
    "cat-file",
    "--filters",
    `--path=${repositoryPath(root, path)}`,
    id,
  ]);
}

export function collectStaged(cwd: string, hunks = false): StagedSelection {
  if (!hasGit()) return { ok: false, error: "--staged needs git, which was not found on PATH" };

  const location = locate(cwd);
  if (location.kind === "failed") return { ok: false, error: location.error };
  if (location.kind === "outside") return { ok: false, error: "not inside a git repository" };

  const root = location.root;
  const range = ["--cached", "--diff-filter=ACMT"];
  const diff = runGit(root, ["diff", ...range, "--raw", "-z", "--no-renames", "--no-abbrev"]);
  if (!diff.ok) return diff;

  const items = nulItems(diff.output);
  const blobOf = new Map<string, string>();
  const edited: string[] = [];
  for (let index = 0; index + 1 < items.length; index += 2) {
    const [, mode, , blob, status] = items[index]!.split(" ");
    const name = items[index + 1]!;
    if (
      mode === undefined ||
      blob === undefined ||
      !regularFileModes.has(mode) ||
      !isCandidate(name)
    )
      continue;

    blobOf.set(resolve(root, name), blob);
    if (status === "M") edited.push(name);
  }

  const changed =
    hunks && edited.length > 0 ? diffLines(root, range, edited.length, edited) : undefined;

  if (changed && !changed.ok) return changed;

  const linesOf = new Map(
    edited.map((name, index) => [resolve(root, name), changed?.lines[index]]),
  );

  const attributes = readAttributes(
    [...blobOf.keys()],
    root,
    ["linguist-generated", "filter"],
    "index",
  );

  if (!attributes.ok) return attributes;

  const unstaged = runGit(root, ["diff", "--name-only", "-z", "--no-renames"]);
  if (!unstaged.ok) return unstaged;

  const dirty = new Set(nulItems(unstaged.output).map((path) => resolve(root, path)));
  const paths = sorted([...blobOf.keys()]).filter(
    (path) => !isSet(attribute(attributes.values, root, path, "linguist-generated")),
  );

  const filtered = (path: string) =>
    !["unspecified", "unset"].includes(
      attribute(attributes.values, root, path, "filter") ?? "unset",
    );

  const plain = paths.filter((path) => !filtered(path));
  const read = readBlobs(
    root,
    plain.map((path) => blobOf.get(path)!),
  );

  if (!read.ok) return read;

  const bytes = new Map(plain.map((path, index) => [path, read.blobs[index]!]));
  const smudge = paths.filter(filtered);
  const worktree = readAttributes(smudge, root, ["filter"], "worktree");
  if (!worktree.ok) return worktree;

  const diverged = smudge.some(
    (path) =>
      attribute(worktree.values, root, path, "filter") !==
      attribute(attributes.values, root, path, "filter"),
  );

  const tree = diverged ? runGit(root, ["write-tree"]) : undefined;
  if (tree && !tree.ok) return tree;

  for (const path of smudge) {
    const smudged = readFilteredBlob(root, tree?.output.trim(), path, blobOf.get(path)!);
    if (!smudged.ok) return smudged;
    bytes.set(path, smudged.output);
  }

  return {
    ok: true,
    files: paths.map((path) => ({
      path,
      bytes: bytes.get(path)!,
      unstaged: dirty.has(path),
      lines: filtered(path) ? undefined : linesOf.get(path),
    })),
  };
}
