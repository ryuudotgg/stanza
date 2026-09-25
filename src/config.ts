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

const BIOME_MENTION = /"useBlockStatements"\s*:/;
const BIOME_ON = /"useBlockStatements"\s*:(?!\s*("off"|\{\s*"level"\s*:\s*"off"))/;

const ESLINT_MENTION = /["']?curly["']?\s*:/;
const ESLINT_ON = /["']?curly["']?\s*:(?!\s*\[?\s*(["']off["']|0\b))/;

const cache = new Map<string, boolean>();

function withoutComments(text: string): string {
  let out = "";

  let quote: string | null = null;
  for (let index = 0; index < text.length; index++) {
    const char = text[index]!;
    const pair = text.slice(index, index + 2);

    if (quote) {
      out += char;
      if (char === "\\") out += text[++index] ?? "";
      else if (char === quote) quote = null;

      continue;
    }

    if (pair === "/*") {
      const close = text.indexOf("*/", index + 2);
      index = close === -1 ? text.length : close + 1;
      continue;
    }

    if (pair === "//") {
      const newline = text.indexOf("\n", index);
      index = newline === -1 ? text.length : newline - 1;
      continue;
    }

    if (char === '"' || char === "'" || char === "`") quote = char;
    out += char;
  }

  return out;
}

function settingIn(dir: string): boolean | null {
  for (const [names, mention, on] of [
    [BIOME_FILES, BIOME_MENTION, BIOME_ON],
    [ESLINT_FILES, ESLINT_MENTION, ESLINT_ON],
  ] as const)
    for (const name of names) {
      const file = join(dir, name);
      if (!existsSync(file)) continue;

      const text = withoutComments(readFileSync(file, "utf8"));
      if (mention.test(text)) return on.test(text);
    }

  return null;
}

export function bracesEnforced(dir: string): boolean {
  const cached = cache.get(dir);
  if (cached !== undefined) return cached;

  const own = settingIn(dir);
  const parent = dirname(dir);
  const result = own ?? (parent === dir ? false : bracesEnforced(parent));

  cache.set(dir, result);
  return result;
}
