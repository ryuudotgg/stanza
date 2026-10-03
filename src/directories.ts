import { existsSync, readdirSync, realpathSync } from "node:fs";
import { basename, dirname, join } from "node:path";

type Listing = { names: Set<string>; folded: Set<string> } | null;

const maps: Map<unknown, unknown>[] = [];
export function invocationMap<K, V>(): Map<K, V> {
  const map = new Map<K, V>();
  maps.push(map);
  return map;
}

export function beginInvocation(): void {
  for (const map of maps) map.clear();
}

const realDirectories = invocationMap<string, string>();
const listings = invocationMap<string, Listing>();
const existence = invocationMap<string, boolean>();
const realPaths = invocationMap<string, string>();

export function realDirectory(dir: string): string {
  const cached = realDirectories.get(dir);
  if (cached !== undefined) return cached;

  const parent = dirname(dir);
  const real = existsSync(dir)
    ? realpathSync(dir)
    : parent === dir
      ? dir
      : join(realDirectory(parent), basename(dir));

  realDirectories.set(dir, real);
  realDirectories.set(real, real);
  return real;
}

function listing(dir: string): Listing {
  const cached = listings.get(dir);
  if (cached !== undefined) return cached;

  let result: Listing;
  try {
    const names = readdirSync(dir);
    result = { names: new Set(names), folded: new Set(names.map((name) => name.toLowerCase())) };
  } catch {
    result = null;
  }

  listings.set(dir, result);
  return result;
}

export function holds(dir: string, name: string): boolean {
  const path = join(dir, name);
  const cached = existence.get(path);
  if (cached !== undefined) return cached;

  const entries = listing(dir);
  const exists = (entries === null || entries.folded.has(name.toLowerCase())) && existsSync(path);

  existence.set(path, exists);
  return exists;
}

export function listed(dir: string, name: string): boolean {
  return listing(dir)?.names.has(name) ?? false;
}

export function realPath(path: string): string {
  const cached = realPaths.get(path);
  if (cached !== undefined) return cached;

  const real = realpathSync(path);
  realPaths.set(path, real);
  return real;
}
