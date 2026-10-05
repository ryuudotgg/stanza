import {
  accessSync,
  closeSync,
  constants,
  existsSync,
  lstatSync,
  openSync,
  readdirSync,
  readFileSync,
  readSync,
  realpathSync,
  statSync,
  type Stats,
} from "node:fs";
import { basename, delimiter, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { invocationMap, listed, realDirectory } from "./directories.ts";
import { entryFor, isCandidate, prunes } from "./languages/index.ts";
import { ignoredByRules, parseIgnore, type Rule } from "./gitignore.ts";
import { diffChanges } from "./engine/hunks.ts";
import type { Changed } from "./engine/model.ts";

export interface Collected {
  files: string[];
  errors: string[];
  warnings: string[];
}

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

export type EmptyReason = "generated" | "ignored" | "skipped" | "none";

export function emptyReason(paths: string[], cwd: string): EmptyReason {
  if (collectFiles(paths, cwd, true).files.length > 0) return "generated";

  let sawSupported = false;
  function candidate(path: string): boolean {
    if (entryFor(path) === undefined) return false;
    sawSupported = true;
    return isCandidate(path, true);
  }

  function walk(dir: string, root: string): boolean {
    let entries;
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      return false;
    }

    for (const entry of entries) {
      const path = join(dir, entry.name);
      const rel = relative(root, path).split(sep).join("/");
      if (entry.isDirectory()) {
        if (entry.name === ".git" || (sawSupported && prunes(rel))) continue;
        if (walk(path, root)) return true;
      } else if (entry.isFile() && candidate(rel)) return true;
    }

    return false;
  }

  for (const input of paths) {
    const path = isAbsolute(input) ? input : resolve(cwd, input);

    let kind;
    try {
      kind = statSync(path);
    } catch {
      continue;
    }

    if (kind.isFile()) sawSupported ||= entryFor(path) !== undefined;
    else if (kind.isDirectory() && walk(path, path)) return "ignored";
  }

  return sawSupported ? "skipped" : "none";
}

const generatedMarkers = [
  /@generated\b/,
  /^code generated\b.*\bdo not edit\b/i,
  /\b(?:this|the) (?:file|code|class) (?:is|was|has been) (?:auto(?:matically)?[- ]?)?generated\b/i,
  /\bauto(?:matically)?[- ]?generated (?:file|code|class)\b/i,
  /^(?:auto(?:matically)?[- ]?)generated\b/i,
];

const generatorAttribution = /^(?:code )?generated (?:by|from|with|using|code)\b/i;

const editWarning = /\bdo not (?:edit|modify|make (?:direct )?changes)\b/i;

export function isGeneratedHeader(text: string): boolean {
  const comments = headerComments(text);
  if (comments.some((comment) => generatedMarkers.some((marker) => marker.test(comment))))
    return true;

  return (
    comments.some((comment) => generatorAttribution.test(comment)) &&
    comments.some((comment) => editWarning.test(comment))
  );
}

function headerComments(text: string): string[] {
  const comments: string[] = [];

  let inBlock = false;
  for (const line of text.split(/\r?\n/, 10)) {
    let rest = line.trim();
    while (rest !== "") {
      if (!inBlock) {
        if (rest.startsWith("//")) {
          comments.push(commentBody(rest.slice(2)));
          break;
        }

        if (!rest.startsWith("/*")) break;

        rest = rest.slice(2);
      }

      const end = rest.indexOf("*/");
      inBlock = end === -1;
      comments.push(commentBody(inBlock ? rest : rest.slice(0, end)));
      rest = inBlock ? "" : rest.slice(end + 2).trimStart();
    }
  }

  return comments;
}

function commentBody(text: string): string {
  return text.replace(/^[/*]+/, "").trim();
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
      if (prunes(rel)) continue;
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

const locations = invocationMap<string, Location>();

interface GitVariable {
  key: string;
  value: string | undefined;
}

interface GitVariables {
  readable: boolean;
  text: string;
  entries: GitVariable[];
}

const gitVariables = invocationMap<string, GitVariables>();

const discoveryEnvironment = [
  "GIT_DIR",
  "GIT_WORK_TREE",
  "GIT_COMMON_DIR",
  "GIT_OBJECT_DIRECTORY",
  "GIT_CEILING_DIRECTORIES",
  "GIT_TEST_ASSUME_DIFFERENT_OWNER",
];

function parseGitVariable(line: string): GitVariable {
  const separator = line.indexOf("=");
  if (separator === -1) return { key: line, value: undefined };
  return { key: line.slice(0, separator), value: line.slice(separator + 1) };
}

function listGitVariables(root: string): GitVariables {
  const cached = gitVariables.get(root);
  if (cached !== undefined) return cached;

  const result = Bun.spawnSync(["git", "-C", root, "var", "-l"], {
    env: process.env,
    stderr: "pipe",
  });

  const text = new TextDecoder().decode(result.stdout);
  const variables = {
    readable: result.exitCode === 0 && result.stderr.length === 0,
    text,
    entries: text.replace(/\n$/, "").split("\n").map(parseGitVariable),
  };

  gitVariables.set(root, variables);
  return variables;
}

function plainConfig(root: string): boolean {
  const { readable, text } = listGitVariables(root);
  return (
    readable &&
    !/^(?:core\.worktree|extensions\.[^=]*)(?:=|$)/im.test(text) &&
    !/^core\.bare(?:=(?!(?:false|no|off|0)$)|$)/im.test(text) &&
    !/^core\.repositoryformatversion(?:=(?![01]$)|$)/im.test(text)
  );
}

function plainRepository(
  worktree: string,
  marker: string,
  markerStat: Stats,
  user: number,
): boolean {
  if (lstatSync(worktree).uid !== user || markerStat.uid !== user) return false;

  const head = join(marker, "HEAD");
  if (!lstatSync(head).isFile()) return false;

  const descriptor = openSync(head, "r");
  const bytes = Buffer.alloc(255);

  let text: string;
  try {
    text = bytes.subarray(0, readSync(descriptor, bytes, 0, bytes.length, 0)).toString("latin1");
  } finally {
    closeSync(descriptor);
  }

  if (!/^(?:ref: refs\/|[0-9a-f]{40})/.test(text)) return false;

  accessSync(join(marker, "objects"), constants.X_OK);
  accessSync(join(marker, "refs"), constants.X_OK);
  if (lstatSync(join(marker, "commondir"), { throwIfNoEntry: false }) !== undefined) return false;

  return plainConfig(worktree);
}

function knownRoot(dir: string): string | undefined {
  try {
    if (discoveryEnvironment.some((name) => process.env[name] !== undefined)) return undefined;
    if (Object.keys(process.env).some((name) => name.startsWith("GIT_CONFIG"))) return undefined;

    const user = process.geteuid?.();
    if (user === undefined) return undefined;

    const start = realpathSync(dir);
    const startStat = statSync(start);
    if (!startStat.isDirectory() || /[^\x20-\x7e]/.test(start) || start.split(sep).includes(".git"))
      return undefined;

    const device = startStat.dev;

    let current = start;
    while (true) {
      const marker = join(current, ".git");
      const entry = lstatSync(marker, { throwIfNoEntry: false });
      if (entry !== undefined)
        return entry.isDirectory() && plainRepository(current, marker, entry, user)
          ? current
          : undefined;

      if (lstatSync(join(current, "HEAD"), { throwIfNoEntry: false }) !== undefined)
        return undefined;

      const parent = dirname(current);
      if (parent === current || statSync(parent).dev !== device) return undefined;

      current = parent;
    }
  } catch {
    return undefined;
  }
}

function discoverLocation(dir: string): Location {
  if (!hasGit()) return { kind: "outside" };

  const known = knownRoot(dir);
  if (known !== undefined) return { kind: "repository", root: known };

  const result = runGit(dir, ["rev-parse", "--show-toplevel"]);
  const root = result.ok ? result.output.replace(/\n$/, "") : "";
  if (root) return { kind: "repository", root };
  if (!result.ok && hasGitMarker(dir)) return { kind: "failed", error: result.error };
  return { kind: "outside" };
}

export function locate(dir: string): Location {
  const known = locations.get(dir);
  if (known !== undefined) return known;

  const location = discoverLocation(dir);
  locations.set(dir, location);
  return location;
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

function sourceMaySetAttributes(path: string, names: string[]): boolean {
  const entry = lstatSync(path, { throwIfNoEntry: false });
  if (entry === undefined) return false;
  if (!entry.isFile()) return true;

  const text = readFileSync(path, "utf8");
  return text.includes("[attr]") || names.some((name) => text.includes(name));
}

function indexMayHaveAttributes(root: string): boolean {
  const path = join(root, ".git/index");
  const entry = lstatSync(path, { throwIfNoEntry: false });
  if (entry === undefined) return false;
  if (!entry.isFile()) return true;

  const bytes = readFileSync(path);
  if (bytes.length < 32 || bytes.toString("ascii", 0, 4) !== "DIRC") return true;

  const version = bytes.readUInt32BE(4);
  if (version !== 2 && version !== 3) return true;

  let offset = 12;
  const count = bytes.readUInt32BE(8);
  for (let entryIndex = 0; entryIndex < count; entryIndex++) {
    if (offset + 62 > bytes.length - 20) return true;
    if ((bytes.readUInt32BE(offset + 24) & 0o170000) === 0o040000) return true;

    const extended = (bytes.readUInt16BE(offset + 60) & 0x4000) !== 0;
    if (extended && version !== 3) return true;

    const start = offset + 62 + (extended ? 2 : 0);
    const end = bytes.indexOf(0, start);
    if (end < start || end >= bytes.length - 20) return true;

    const name = bytes.toString("utf8", start, end);
    if (name === ".gitattributes" || name.endsWith("/.gitattributes")) return true;

    offset += Math.ceil((end + 1 - offset) / 8) * 8;
  }

  while (offset < bytes.length - 20) {
    if (offset + 8 > bytes.length - 20) return true;
    if (bytes.toString("ascii", offset, offset + 4) !== "TREE") return true;
    offset += 8 + bytes.readUInt32BE(offset + 4);
  }

  return offset !== bytes.length - 20;
}

// git var -l prints GIT_EDITOR, GIT_SEQUENCE_EDITOR and GIT_PAGER raw, so a newline could forge a line.
const inertGitEnvironment = new Set([
  "GIT_EDITOR",
  "GIT_SEQUENCE_EDITOR",
  "GIT_PAGER",
  "GIT_TERMINAL_PROMPT",
  "GIT_ASKPASS",
  "GIT_SSH",
  "GIT_SSH_COMMAND",
  "GIT_SSH_VARIANT",
  "GIT_AUTHOR_NAME",
  "GIT_AUTHOR_EMAIL",
  "GIT_AUTHOR_DATE",
  "GIT_COMMITTER_NAME",
  "GIT_COMMITTER_EMAIL",
  "GIT_COMMITTER_DATE",
  "GIT_OPTIONAL_LOCKS",
]);

function environmentMayAffectAttributes(): boolean {
  return Object.entries(process.env).some(
    ([name, value]) =>
      name.startsWith("GIT_") && (!inertGitEnvironment.has(name) || value?.includes("\n") === true),
  );
}

function rebuildablePath(path: string | undefined): path is string {
  return (
    path !== undefined && (path.startsWith("~/") || !path.startsWith("~")) && !path.includes("%(")
  );
}

function normalizeAsText(path: string): string {
  return path
    .split("/")
    .filter((segment, position) => position === 0 || (segment !== "" && segment !== "."))
    .join("/");
}

function defaultGlobalAttributes(): string | undefined {
  const { HOME: home, XDG_CONFIG_HOME: configHome } = process.env;
  if (configHome) return `${configHome}/git/attributes`;
  return home === undefined ? undefined : `${home}/.config/git/attributes`;
}

function expectedGlobalAttributes(configured: (string | undefined)[]): string | undefined {
  if (!configured.every(rebuildablePath)) return undefined;

  const path = configured.at(-1);
  const home = process.env.HOME;
  if (path === undefined) return defaultGlobalAttributes();
  if (!path.startsWith("~/")) return path;
  return home === undefined ? undefined : `${home}/${path.slice(2)}`;
}

const attributeVariableOrder = ["GIT_ATTR_SYSTEM", "GIT_ATTR_GLOBAL", "GIT_CONFIG_SYSTEM"];
// Once each, adjacent and in git's order, so neither path value spans or forges a line.
function attributeVariableValues(entries: GitVariable[]): (string | undefined)[] | undefined {
  const sequence = entries.flatMap((entry, position) =>
    attributeVariableOrder.includes(entry.key) ? [{ ...entry, position }] : [],
  );

  const start = sequence[0]?.position;
  const exact =
    sequence.length === attributeVariableOrder.length &&
    sequence.every(
      (entry, offset) =>
        entry.key === attributeVariableOrder[offset] && entry.position - offset === start,
    );

  return exact ? sequence.map((entry) => entry.value) : undefined;
}

function attributeSourcePaths(root: string): string[] | undefined {
  const variables = gitVariables.get(root);
  if (variables === undefined || !variables.readable) return undefined;

  const { entries } = variables;
  if (entries.some(({ key }) => /^attr\./i.test(key))) return undefined;

  const [system, global] = attributeVariableValues(entries) ?? [];
  if (system === undefined || global === undefined) return undefined;

  const configured = entries
    .filter(({ key }) => key.toLowerCase() === "core.attributesfile")
    .map(({ value }) => value);

  // git var collapses .. as text but check-attr opens the raw path, where a symlink resolves first.
  const expected = expectedGlobalAttributes(configured);
  if (expected === undefined || normalizeAsText(expected) !== global) return undefined;

  const paths = [system, global];
  return paths.every(isAbsolute) ? paths : undefined;
}

function attributesMayApply(files: string[], root: string, names: string[]): boolean {
  if (environmentMayAffectAttributes()) return true;

  const sources = attributeSourcePaths(root);
  if (sources === undefined) return true;

  try {
    if (indexMayHaveAttributes(root)) return true;

    const paths = new Set([...sources, join(root, ".git/info/attributes")]);
    for (const file of files) {
      if (!within(root, file)) return true;

      for (let directory = dirname(file); ; directory = dirname(directory)) {
        paths.add(join(directory, ".gitattributes"));
        if (directory === root) break;
        if (!within(root, directory)) return true;
      }
    }

    return [...paths].some((path) => sourceMaySetAttributes(path, names));
  } catch {
    return true;
  }
}

function readAttributes(
  files: string[],
  root: string,
  names: string[],
  source: "worktree" | "index",
): Attributes {
  if (files.length === 0) return { ok: true, values: new Map() };
  if (
    source === "worktree" &&
    names.length === 1 &&
    names[0] === "linguist-generated" &&
    !attributesMayApply(files, root, names)
  )
    return { ok: true, values: new Map() };

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
    .filter((path) => {
      try {
        return lstatSync(path).isFile();
      } catch {
        return false;
      }
    });

  const inside = files.map((path) => listedFileRealpath(path)).filter((path) => within(root, path));
  return { ok: true, files: inside };
}

// realpathSync returns the name in its on disk case and normalization, so only an exact listing match may join.
function listedFileRealpath(path: string): string {
  const dir = dirname(path);
  const name = basename(path);
  return listed(dir, name) ? join(realDirectory(dir), name) : fileRealpath(path);
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
  const byRoot = new Map<string, string[]>();
  const outside: string[] = [];
  const errors: string[] = [];
  const warnings = hasGit()
    ? []
    : ["git not found on PATH, file selection fell back to the directory walk"];

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
      const location = locate(path);
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

    if (entryFor(path) === undefined) {
      errors.push(`not a TypeScript or JavaScript file: ${input}`);
      continue;
    }

    const file = fileRealpath(path);
    const location = locate(dirname(file));
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
  if (entryFor(path) === undefined)
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

const pathspecBudget = 64 * 1024;
export function collectChanged(
  cwd: string,
  location: Location = locate(cwd),
  hunks = false,
  written?: Set<string>,
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
  const realRoot = realpathSync(root);
  const scope = [...(written ?? [])]
    .filter((path) => within(realRoot, path))
    .map((path) => repositoryPath(realRoot, path));

  const pathspecs = Buffer.byteLength(scope.join("")) <= pathspecBudget ? scope : [];

  let changed = runGit(root, [
    "--literal-pathspecs",
    "diff",
    "--name-only",
    "-z",
    "--no-renames",
    "--submodule=short",
    "HEAD",
    "--",
    ...pathspecs,
  ]);

  const born = changed.ok || runGit(root, ["rev-parse", "--verify", "-q", "HEAD"]).ok;
  if (!born) {
    const branch = runGit(root, ["symbolic-ref", "-q", "HEAD"]);
    if (!branch.ok) return { files: [], errors: [branch.error], warnings: [], changedLines };

    changed = runGit(root, [
      "--literal-pathspecs",
      "ls-files",
      "-z",
      "--cached",
      "--",
      ...pathspecs,
    ]);
  }

  const untracked = runGit(root, [
    "--literal-pathspecs",
    "ls-files",
    "-z",
    "--others",
    "--exclude-standard",
    "--",
    ...pathspecs,
  ]);

  if (!changed.ok || !untracked.ok) {
    const errors = [changed, untracked].flatMap((result) => (result.ok ? [] : [result.error]));
    return { files: [], errors: [...new Set(errors)], warnings: [], changedLines };
  }

  const kept = (name: string) => written === undefined || written.has(resolve(realRoot, name));
  const names = nulItems(changed.output).filter(kept);
  const candidates = names.filter((name) => isCandidate(name));
  if (hunks && born && candidates.length > 0) {
    const diff = diffLines(root, ["HEAD"], candidates.length, candidates);
    if (!diff.ok) return { files: [], errors: [diff.error], warnings: [], changedLines };

    for (let index = 0; index < candidates.length; index++)
      changedLines.set(resolve(root, candidates[index]!), diff.lines[index]!);
  }

  const untrackedNames = nulItems(untracked.output).filter(kept);
  for (const name of untrackedNames) changedLines.delete(resolve(root, name));

  const files = [...names, ...untrackedNames]
    .filter((path) => isCandidate(path))
    .map((path) => resolve(root, path))
    .filter((path) => existsSync(path) && lstatSync(path).isFile())
    .filter((path) => within(realRoot, fileRealpath(path)));

  const generated = dropGeneratedAttributes(files, root);
  if (!generated.ok) return { files: [], errors: [generated.error], warnings: [], changedLines };

  return { files: sorted(generated.files), errors: [], warnings: [], changedLines };
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
