import { resolve } from "node:path";
import { biome } from "./biome.ts";
import { flat, legacy } from "./eslint.ts";
import { realDirectory } from "./find.ts";
import type { Decision, Reader } from "./layers.ts";
import { oxlint } from "./oxlint.ts";

const READERS: Reader[] = [flat, legacy, biome, oxlint];

export interface BracesSetting {
  enforced: boolean;
  unread: string[];
}

const cache = new Map<string, BracesSetting>();

export function braceDecisions(dir: string, extension?: string): Decision[] {
  dir = realDirectory(resolve(dir));
  return READERS.map((reader) => reader.decide(dir, extension)).filter(
    (decision) => decision !== null,
  );
}

export function bracesSetting(dir: string, extension?: string): BracesSetting {
  const requested = `${dir}\0${extension ?? ""}`;
  const cached = cache.get(requested);
  if (cached !== undefined) return cached;

  let result: BracesSetting = { enforced: true, unread: [] };
  try {
    const decisions = braceDecisions(dir, extension);
    const enforced = decisions.some((decision) => decision.setting !== "off");
    const unread = decisions.some((decision) => decision.setting === "on")
      ? []
      : decisions
          .filter((decision) => decision.setting === "unknown")
          .flatMap((decision) => decision.files);

    result = { enforced, unread };
  } catch {}

  cache.set(requested, result);
  return result;
}

export function bracesEnforced(dir: string, extension?: string): boolean {
  return bracesSetting(dir, extension).enforced;
}
