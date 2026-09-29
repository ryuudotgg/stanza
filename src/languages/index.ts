import { basename, extname } from "node:path";
import { entry as javascript } from "./javascript/entry.ts";
import type { Entry, Language } from "./language.ts";

const entries: Entry[] = [javascript];
const byExtension = new Map(
  entries.flatMap((entry) => entry.extensions.map((extension) => [extension, entry] as const)),
);

export function entryFor(path: string): Entry | undefined {
  return byExtension.get(extname(path));
}

export function languageOf(path: string): Language {
  const entry = entryFor(path);
  if (!entry) throw new Error(`no language is registered for ${path}`);
  return entry.load();
}

export function isCandidate(relativePath: string, keepGenerated = false): boolean {
  const entry = entryFor(relativePath);
  return (
    entry !== undefined &&
    !entry.skipsName(basename(relativePath), keepGenerated) &&
    !relativePath.split(/[\\/]+/).some((segment) => entry.directories.has(segment))
  );
}

export function prunes(relativeDirectory: string): boolean {
  const segments = relativeDirectory.split(/[\\/]+/);
  return entries.every((entry) => segments.some((segment) => entry.directories.has(segment)));
}
