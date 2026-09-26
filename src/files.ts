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

type Selection = { ok: true; files: string[] } | { ok: false; error: string };

function runGit(cwd: string, args: string[], stdin?: Uint8Array): GitResult {
  const result = Bun.spawnSync(["git", "-C", cwd, ...args], { stdin, stderr: "pipe" });
  if (result.exitCode === 0) return { ok: true, output: new TextDecoder().decode(result.stdout) };

  const reason =
    new TextDecoder()
      .decode(result.stderr)
      .split("\n")
      .map((line) => line.trim())
      .find(Boolean) ?? `exit ${result.exitCode}`;

  return { ok: false, error: `git ${args[0]} failed in ${cwd}: ${reason}` };
}

function nulItems(text: string): string[] {
  return text.split("\0").filter(Boolean);
}

function supported(path: string): boolean {
  return extensions.has(extname(path));
}

function skippedName(path: string): boolean {
  const name = basename(path);
  return (
    name.endsWith(".d.ts") ||
    name.endsWith(".d.mts") ||
    name.endsWith(".d.cts") ||
    name.endsWith(".gen.ts") ||
    name.endsWith(".gen.tsx") ||
    name.includes(".generated.") ||
    name.endsWith(".min.js")
  );
}

function hasSkippedSegment(path: string): boolean {
  return path.split(/[\\/]+/).some((segment) => skippedSegments.has(segment));
}

export function isCandidate(relativePath: string): boolean {
  return supported(relativePath) && !skippedName(relativePath) && !hasSkippedSegment(relativePath);
}

export function isGeneratedHeader(text: string): boolean {
  return text
    .split(/\r?\n/, 10)
    .some(
      (line) => line.includes("@generated") || /DO NOT EDIT|automatically generated/i.test(line),
    );
}

function ignoredBy(pattern: string, path: string): boolean {
  const anchored = pattern.startsWith("/");
  const directory = pattern.endsWith("/");
  const source = pattern.replace(/^\//, "").replace(/\/$/, "");
  const escaped = source
    .split(/(\*\*\/|\*\*)/)
    .map((part) => {
      if (part === "**/") return "(?:.*/)?";
      if (part === "**") return ".*";
      return part.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, "[^/]*");
    })
    .join("");

  const prefix = anchored || source.includes("/") ? "^" : "(^|.*/)";
  const suffix = directory ? "(/|$)" : "$";
  return new RegExp(`${prefix}${escaped}${suffix}`).test(path);
}

function ignored(path: string, patterns: string[]): boolean {
  return patterns.some((pattern) => ignoredBy(pattern, path));
}

function fallbackFiles(dir: string): string[] {
  const ignoreFile = join(dir, ".gitignore");
  const patterns = existsSync(ignoreFile)
    ? readFileSync(ignoreFile, "utf8")
        .split(/\r?\n/)
        .filter((line) => line && !line.startsWith("#") && !line.startsWith("!"))
    : [];

  const files: string[] = [];
  function walk(current: string): void {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const file = join(current, entry.name);
      const rel = relative(dir, file).split(sep).join("/");
      if (hasSkippedSegment(rel) || ignored(rel, patterns)) continue;

      if (entry.isDirectory()) walk(file);
      else if (entry.isFile() && isCandidate(rel)) files.push(resolve(file));
    }
  }

  walk(dir);
  return files;
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

function dropGeneratedAttributes(files: string[], root: string): Selection {
  if (files.length === 0) return { ok: true, files };

  const paths = files.map((file) => relative(root, file).split(sep).join("/")).join("\0") + "\0";
  const result = runGit(
    root,
    ["check-attr", "linguist-generated", "-z", "--stdin"],
    new TextEncoder().encode(paths),
  );

  if (!result.ok) return result;

  const generated = new Set<string>();
  const values = nulItems(result.output);
  for (let index = 0; index + 2 < values.length; index += 3) {
    const path = values[index];
    const value = values[index + 2];
    if (path !== undefined && (value === "true" || value === "set")) generated.add(path);
  }

  return {
    ok: true,
    files: files.filter((file) => !generated.has(relative(root, file).split(sep).join("/"))),
  };
}

function directoryFiles(dir: string, root: string): Selection {
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
    .filter((path) => isCandidate(path))
    .map((path) => resolve(dir, path))
    .filter((path) => existsSync(path) && lstatSync(path).isFile());

  const inside = files
    .map((path) => realpathSync(path))
    .filter((path) => !relative(root, path).startsWith(`..${sep}`));

  return { ok: true, files: inside };
}

function sorted(files: string[]): string[] {
  return [...new Set(files)].sort((left, right) => left.localeCompare(right));
}

export function collectFiles(paths: string[], cwd: string): Collected {
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

    if (statSync(path).isDirectory()) {
      const location = locationOf(path);
      if (location.kind === "failed") {
        errors.push(location.error);
        continue;
      }

      if (location.kind === "outside") {
        outside.push(...fallbackFiles(path));
        continue;
      }

      const listed = directoryFiles(path, location.root);
      if (listed.ok) add(location.root, listed.files);
      else errors.push(listed.error);

      continue;
    }

    if (!supported(path)) {
      errors.push(`not a TypeScript or JavaScript file: ${input}`);
      continue;
    }

    if (isCandidate(relative(cwd, path) || basename(path))) {
      const file = realpathSync(path);
      const location = locationOf(dirname(file));
      if (location.kind === "failed") errors.push(location.error);
      else if (location.kind === "outside") outside.push(file);
      else add(location.root, [file]);
    }
  }

  const files = [...outside];
  for (const [root, candidates] of byRoot) {
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
  | { status: "format"; path: string }
  | { status: "skip" }
  | { status: "unsupported"; error: string }
  | { status: "failed"; error: string };

export function stdinTarget(input: string, cwd: string): StdinTarget {
  const path = isAbsolute(input) ? input : resolve(cwd, input);
  if (!supported(path))
    return { status: "unsupported", error: `not a TypeScript or JavaScript file: ${input}` };
  if (!isCandidate(relative(cwd, path) || basename(path))) return { status: "skip" };

  const directory = existingAncestor(dirname(path));
  const real = realpathSync(directory);
  const file = join(real, relative(directory, path));

  const location = locate(real);
  if (location.kind === "failed") return { status: "failed", error: location.error };
  if (location.kind === "outside") return { status: "format", path: file };

  const kept = dropGeneratedAttributes([file], location.root);
  if (!kept.ok) return { status: "failed", error: kept.error };
  if (kept.files.length === 0) return { status: "skip" };

  return { status: "format", path: file };
}

export function collectChanged(cwd: string, location: Location = locate(cwd)): Collected {
  if (!hasGit())
    return {
      files: [],
      errors: ["--changed needs git, which was not found on PATH"],
      warnings: [],
    };

  if (location.kind === "failed") return { files: [], errors: [location.error], warnings: [] };
  if (location.kind === "outside")
    return { files: [], errors: ["not inside a git repository"], warnings: [] };

  const root = location.root;

  const born = runGit(root, ["rev-parse", "--verify", "-q", "HEAD"]).ok;
  if (!born) {
    const branch = runGit(root, ["symbolic-ref", "-q", "HEAD"]);
    if (!branch.ok) return { files: [], errors: [branch.error], warnings: [] };
  }

  const changed = born
    ? runGit(root, ["diff", "--name-only", "-z", "HEAD", "--"])
    : runGit(root, ["ls-files", "-z", "--cached"]);

  const untracked = runGit(root, ["ls-files", "-z", "--others", "--exclude-standard"]);
  if (!changed.ok || !untracked.ok) {
    const errors = [changed, untracked].flatMap((result) => (result.ok ? [] : [result.error]));
    return { files: [], errors: [...new Set(errors)], warnings: [] };
  }

  const files = [...nulItems(changed.output), ...nulItems(untracked.output)]
    .filter((path) => isCandidate(path))
    .map((path) => resolve(root, path))
    .filter((path) => existsSync(path) && lstatSync(path).isFile());

  const kept = dropGeneratedAttributes(files, root);
  if (!kept.ok) return { files: [], errors: [kept.error], warnings: [] };

  return { files: sorted(kept.files), errors: [], warnings: [] };
}
