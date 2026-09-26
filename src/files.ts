import { existsSync, lstatSync, readdirSync, readFileSync, realpathSync, statSync } from "node:fs";
import { basename, dirname, extname, isAbsolute, join, relative, resolve, sep } from "node:path";

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

function runGit(cwd: string, args: string[], stdin?: Uint8Array): { ok: boolean; output: string } {
  if (!hasGit()) return { ok: false, output: "" };
  const result = Bun.spawnSync(["git", "-C", cwd, ...args], { stdin });
  return { ok: result.exitCode === 0, output: new TextDecoder().decode(result.stdout) };
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

function repository(dir: string): string | undefined {
  const result = runGit(dir, ["rev-parse", "--show-toplevel"]);
  return result.ok ? result.output.trim() : undefined;
}

function dropGeneratedAttributes(files: string[], root: string): string[] {
  if (files.length === 0) return files;

  const paths = files.map((file) => relative(root, file).split(sep).join("/")).join("\0") + "\0";
  const result = runGit(
    root,
    ["check-attr", "linguist-generated", "-z", "--stdin"],
    new TextEncoder().encode(paths),
  );

  if (!result.ok) return files;

  const generated = new Set<string>();
  const values = nulItems(result.output);
  for (let index = 0; index + 2 < values.length; index += 3) {
    const path = values[index];
    const value = values[index + 2];
    if (path !== undefined && (value === "true" || value === "set")) generated.add(path);
  }

  return files.filter((file) => !generated.has(relative(root, file).split(sep).join("/")));
}

function directoryFiles(dir: string): string[] {
  const root = repository(dir);
  if (!root) return fallbackFiles(dir);

  const result = runGit(dir, [
    "ls-files",
    "-z",
    "--cached",
    "--others",
    "--exclude-standard",
    "--",
    ".",
  ]);

  const files = nulItems(result.output)
    .filter((path) => isCandidate(path))
    .map((path) => resolve(dir, path))
    .filter((path) => existsSync(path) && lstatSync(path).isFile());

  return dropGeneratedAttributes(
    files.map((path) => realpathSync(path)),
    root,
  );
}

function sorted(files: string[]): string[] {
  return [...new Set(files)].sort((left, right) => left.localeCompare(right));
}

export function collectFiles(paths: string[], cwd: string): Collected {
  const files: string[] = [];
  const errors: string[] = [];
  const warnings = hasGit()
    ? []
    : ["git not found on PATH, file selection fell back to the directory walk"];

  for (const input of paths) {
    const path = isAbsolute(input) ? input : resolve(cwd, input);
    if (!existsSync(path)) {
      errors.push(`no such file: ${input}`);
      continue;
    }

    if (statSync(path).isDirectory()) {
      files.push(...directoryFiles(path));
      continue;
    }

    if (!supported(path)) {
      errors.push(`not a TypeScript or JavaScript file: ${input}`);
      continue;
    }

    if (isCandidate(relative(cwd, path) || basename(path))) {
      const file = realpathSync(path);
      const root = repository(dirname(file));
      files.push(...(root ? dropGeneratedAttributes([file], root) : [file]));
    }
  }

  return { files: sorted(files), errors, warnings };
}

function existingAncestor(path: string): string {
  const parent = dirname(path);
  return existsSync(path) || parent === path ? path : existingAncestor(parent);
}

export type StdinTarget =
  | { status: "format"; path: string }
  | { status: "skip" }
  | { status: "unsupported"; error: string };

export function stdinTarget(input: string, cwd: string): StdinTarget {
  const path = isAbsolute(input) ? input : resolve(cwd, input);
  if (!supported(path))
    return { status: "unsupported", error: `not a TypeScript or JavaScript file: ${input}` };
  if (!isCandidate(relative(cwd, path) || basename(path))) return { status: "skip" };

  const directory = existingAncestor(dirname(path));
  const real = realpathSync(directory);
  const file = existsSync(path) ? realpathSync(path) : join(real, relative(directory, path));
  const root = repository(real);
  if (root && dropGeneratedAttributes([file], root).length === 0) return { status: "skip" };

  return { status: "format", path: file };
}

export function collectChanged(cwd: string): Collected {
  if (!hasGit())
    return {
      files: [],
      errors: ["--changed needs git, which was not found on PATH"],
      warnings: [],
    };

  const root = repository(cwd);
  if (!root) return { files: [], errors: ["not inside a git repository"], warnings: [] };

  const born = runGit(root, ["rev-parse", "--verify", "-q", "HEAD"]).ok;
  const changed = born
    ? runGit(root, ["diff", "--name-only", "-z", "HEAD", "--"])
    : runGit(root, ["ls-files", "-z", "--cached"]);

  const untracked = runGit(root, ["ls-files", "-z", "--others", "--exclude-standard"]);
  const files = [...nulItems(changed.output), ...nulItems(untracked.output)]
    .filter((path) => isCandidate(path))
    .map((path) => resolve(root, path))
    .filter((path) => existsSync(path) && lstatSync(path).isFile());

  return { files: sorted(dropGeneratedAttributes(files, root)), errors: [], warnings: [] };
}
