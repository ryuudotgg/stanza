import { resolve } from "node:path";
import { biome } from "./biome.ts";
import { flat, legacy } from "./eslint.ts";
import { realDirectory } from "./find.ts";
import type { Decision, Family, Reader } from "./layers.ts";
import { oxlint } from "./oxlint.ts";

const READERS: Reader[] = [flat, legacy, biome, oxlint];

export const CONFIG_FILES: readonly {
  readonly family: Family;
  readonly files: readonly string[];
}[] = READERS;

const cache = new Map<string, boolean>();

export function braceDecisions(dir: string, extension?: string): Decision[] {
  dir = realDirectory(resolve(dir));
  return READERS.map((reader) => reader.decide(dir, extension)).filter(
    (decision) => decision !== null,
  );
}

export function bracesEnforced(dir: string, extension?: string): boolean {
  const requested = `${dir}\0${extension ?? ""}`;
  const cached = cache.get(requested);
  if (cached !== undefined) return cached;

  let result = true;
  try {
    result = braceDecisions(dir, extension).some((decision) => decision.setting !== "off");
  } catch {}

  cache.set(requested, result);
  return result;
}
