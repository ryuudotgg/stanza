import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";

const BIOME_FILES = ["biome.json", "biome.jsonc"];
const ESLINT_FILES = [
  "eslint.config.js",
  "eslint.config.mjs",
  "eslint.config.cjs",
  "eslint.config.ts",
  "eslint.config.mts",
  ".eslintrc",
  ".eslintrc.js",
  ".eslintrc.cjs",
  ".eslintrc.json",
  ".eslintrc.yml",
  ".eslintrc.yaml",
];

const BIOME_RULE = /"useBlockStatements"\s*:(?!\s*("off"|\{\s*"level"\s*:\s*"off"))/;
const ESLINT_RULE = /["']?curly["']?\s*:(?!\s*\[?\s*(["']off["']|0\b))/;

const cache = new Map<string, boolean>();

function enforcedIn(dir: string): boolean | null {
  let found = false;
  for (const [names, rule] of [
    [BIOME_FILES, BIOME_RULE],
    [ESLINT_FILES, ESLINT_RULE],
  ] as const)
    for (const name of names) {
      const file = join(dir, name);
      if (!existsSync(file)) continue;

      found = true;
      if (rule.test(readFileSync(file, "utf8"))) return true;
    }

  return found ? false : null;
}

export function bracesEnforced(dir: string): boolean {
  const cached = cache.get(dir);
  if (cached !== undefined) return cached;

  const own = enforcedIn(dir);
  const parent = dirname(dir);
  const result = own ?? (parent === dir ? false : bracesEnforced(parent));

  cache.set(dir, result);
  return result;
}
