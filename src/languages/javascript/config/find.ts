import { readFileSync, realpathSync, statSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { holds } from "../../../directories.ts";

const suffixes = ["", ".json", ".jsonc", ".js", ".cjs", ".mjs", ".ts", ".mts", ".cts", "/index.js"];

const directoryFiles = new Map<string, string[]>();

function fileAt(path: string): string | undefined {
  for (const suffix of suffixes) {
    try {
      const candidate = path + suffix;
      if (statSync(candidate).isFile()) return candidate;
    } catch {}
  }

  if (path.endsWith(".js"))
    return fileAt(path.slice(0, -3) + ".ts") ?? fileAt(path.slice(0, -3) + ".mts");

  return undefined;
}

export type Loader = "import" | "require";

function entryOf(value: unknown, loader: Loader): string | undefined {
  if (typeof value === "string") return value;

  if (value && typeof value === "object" && !Array.isArray(value))
    for (const [key, target] of Object.entries(value))
      if (key === loader || key === "node" || key === "default") {
        const entry = entryOf(target, loader);
        if (entry !== undefined) return entry;
      }

  return undefined;
}

export function resolveModule(specifier: string, from: string, loader: Loader): string | undefined {
  let dir: string;
  try {
    dir = dirname(realpathSync(from));
  } catch {
    return undefined;
  }

  if (specifier.startsWith(".") || isAbsolute(specifier))
    return fileAt(resolve(dirname(from), specifier));

  const parts = specifier.split("/");
  const name = parts.splice(0, specifier.startsWith("@") ? 2 : 1).join("/");
  const subpath = parts.join("/");
  while (true) {
    const root = join(dir, "node_modules", name);
    try {
      const manifest: unknown = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
      if (!manifest || typeof manifest !== "object") return undefined;

      const exports: unknown = Reflect.get(manifest, "exports");
      const target =
        exports && typeof exports === "object"
          ? Reflect.get(exports, subpath ? `./${subpath}` : ".")
          : subpath
            ? undefined
            : exports;

      const entry = entryOf(target, loader) ?? (!subpath ? entryOf(exports, loader) : undefined);
      const main: unknown = Reflect.get(manifest, "main");
      return fileAt(
        join(root, entry ?? (subpath || (typeof main === "string" ? main : "index.js"))),
      );
    } catch {}

    const parent = dirname(dir);
    if (parent === dir) return undefined;

    dir = parent;
  }
}

export function* configDirectories(
  dir: string,
  reader: { family: string; files: readonly string[]; candidate?(file: string): boolean },
): Generator<string[]> {
  while (true) {
    const key = `${reader.family}:${dir}`;

    let files = directoryFiles.get(key);
    if (!files) {
      const current = dir;
      files = reader.files
        .filter((name) => holds(current, name))
        .map((name) => join(current, name))
        .filter((file) => reader.candidate?.(file) ?? true);

      directoryFiles.set(key, files);
    }

    yield files;

    const parent = dirname(dir);
    if (parent === dir) break;

    dir = parent;
  }
}
