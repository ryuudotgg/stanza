import {
  closeSync,
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export interface PageText {
  path: string;
  raw: string;
  processed: string;
}

export interface Docs<Page> {
  pages: string[];
  sidebar: string[];
  texts: Record<string, Page>;
}

const docsDirectory = new URL("../docs/", import.meta.url).pathname;
const repositoryDirectory = new URL("../", import.meta.url).pathname;
type Mode = "text" | "html";

function isProcessAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    const { code } = error as NodeJS.ErrnoException;
    if (code === "ESRCH") return false;
    if (code === "EPERM") return true;

    throw error;
  }
}

function removeAbandonedCaches(): void {
  for (const entry of readdirSync(tmpdir(), { withFileTypes: true })) {
    const match = /^stanza-docs-(\d+)$/.exec(entry.name);
    if (!entry.isDirectory() || !match) continue;

    const pid = Number(match[1]);
    if (!Number.isSafeInteger(pid) || pid <= 0 || isProcessAlive(pid)) continue;

    try {
      rmSync(join(tmpdir(), entry.name), { recursive: true, force: true });
    } catch {}
  }
}

function cacheKey(mode: Mode): string {
  const paths = ["bun.lock", "package.json", "docs/package.json"];
  const roots = [
    "src",
    "tests/fixtures",
    "docs/content",
    "docs/lib",
    "docs/components",
    "docs/scripts",
  ];

  const glob = new Bun.Glob("**/*");
  for (const root of roots) {
    const cwd = join(repositoryDirectory, root);
    if (!existsSync(cwd)) continue;
    for (const path of glob.scanSync({ cwd, onlyFiles: true })) paths.push(join(root, path));
  }

  const hasher = new Bun.CryptoHasher("sha256");
  hasher.update(mode);

  for (const path of paths.sort()) {
    const absolutePath = join(repositoryDirectory, path);
    if (!existsSync(absolutePath)) continue;

    const contents = readFileSync(absolutePath);
    hasher.update(`\0${path}\0${contents.length}\0`);
    hasher.update(contents);
  }

  return hasher.digest("hex");
}

function acquireLock(path: string): boolean {
  let descriptor: number;
  try {
    descriptor = openSync(path, "wx");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "EEXIST") return false;
    throw error;
  }

  try {
    writeFileSync(descriptor, String(process.pid));
  } catch (error) {
    rmSync(path, { force: true });
    throw error;
  } finally {
    closeSync(descriptor);
  }

  return true;
}

function removeDeadLock(path: string): void {
  let holder: string;
  try {
    holder = readFileSync(path, "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return;
    throw error;
  }

  const pid = Number(holder);
  if (!Number.isSafeInteger(pid) || pid <= 0 || isProcessAlive(pid)) return;

  rmSync(path, { force: true });
}

function generatePages(mode: Mode, cachePath: string): string {
  const args = mode === "html" ? ["--html"] : [];
  const result = Bun.spawnSync(
    ["bun", "--preload", "./scripts/preload.ts", "scripts/pages.ts", ...args],
    {
      cwd: docsDirectory,
      env: { ...process.env, DOCS_SKIP_HIGHLIGHT: mode === "text" ? "1" : undefined },
    },
  );

  if (result.exitCode !== 0)
    throw new Error(`Could not load docs pages: ${result.stderr.toString()}`);

  const stdout = result.stdout.toString();

  const temporaryPath = `${cachePath}.${process.pid}.tmp`;
  try {
    writeFileSync(temporaryPath, stdout);
    renameSync(temporaryPath, cachePath);
  } finally {
    rmSync(temporaryPath, { force: true });
  }

  return stdout;
}

function load<Page>(mode: Mode): Docs<Page> {
  removeAbandonedCaches();

  const runId = process.env.BUN_TEST_WORKER_ID !== undefined ? process.ppid : process.pid;
  const directory = join(tmpdir(), `stanza-docs-${runId}`);
  mkdirSync(directory, { recursive: true });

  const key = cacheKey(mode);
  const cachePath = join(directory, `${mode}-${key}.json`);
  const lockPath = join(directory, `${mode}-${key}.lock`);

  const deadline = Date.now() + 120_000;
  while (!existsSync(cachePath)) {
    if (Date.now() >= deadline) throw new Error(`Timed out waiting for ${mode} docs pages cache`);

    if (acquireLock(lockPath)) {
      try {
        if (existsSync(cachePath)) break;
        return JSON.parse(generatePages(mode, cachePath)) as Docs<Page>;
      } finally {
        rmSync(lockPath, { force: true });
      }
    }

    removeDeadLock(lockPath);
    Bun.sleepSync(50);
  }

  return JSON.parse(readFileSync(cachePath, "utf8")) as Docs<Page>;
}

export const loadDocs = () => load<PageText>("text");

export const loadRenderedDocs = () => load<PageText & { html: string }>("html");
