import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import type { Node } from "oxc-parser";
import { children } from "./ast.ts";
import { parse } from "./parse.ts";

interface Linter {
  files: string[];
  rule: string;
}

const LINTERS: Linter[] = [
  { files: ["biome.json", "biome.jsonc"], rule: "useBlockStatements" },
  {
    files: [
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
    ],
    rule: "curly",
  },
];

type Setting = "on" | "off" | "unknown";

const cache = new Map<string, boolean>();

function keyName(node: Node): string | null {
  if (node.type !== "Property") return null;
  if (node.key.type === "Identifier") return node.key.name;
  if (node.key.type === "Literal" && typeof node.key.value === "string") return node.key.value;
  return null;
}

function levelOf(value: Node): Setting {
  if (value.type === "Literal") {
    if (value.value === "off" || value.value === 0) return "off";
    return typeof value.value === "string" || typeof value.value === "number" ? "on" : "unknown";
  }

  if (value.type === "ArrayExpression") {
    const first = value.elements[0];
    return first && first.type !== "SpreadElement" ? levelOf(first) : "unknown";
  }

  if (value.type === "ObjectExpression") {
    const level = value.properties.find((property) => keyName(property) === "level");
    return level?.type === "Property" ? levelOf(level.value) : "unknown";
  }

  return "unknown";
}

function settings(node: Node, rule: string, found: Setting[]): void {
  if (keyName(node) === rule && node.type === "Property") found.push(levelOf(node.value));
  for (const [, child] of children(node)) settings(child, rule, found);
}

function yamlSetting(text: string, rule: string): Setting | null {
  const line = text
    .split(/\r?\n/)
    .find((entry) => new RegExp(`^\\s*["']?${rule}["']?\\s*:`).test(entry));

  if (line === undefined) return null;
  return /:\s*(["']?off["']?|0)\s*(#.*)?$/.test(line) ? "off" : "on";
}

function parsedSetting(file: string, text: string, rule: string): Setting | null {
  const script = /\.[cm]?[jt]s$/.test(file);
  const parsed = parse(script ? file : `${file}.js`, script ? text : `(${text}\n)`);
  if (parsed.errors.length > 0) return "unknown";

  const found: Setting[] = [];
  settings(parsed.program, rule, found);
  if (found.length === 0) return null;
  return found.every((setting) => setting === "off") ? "off" : "on";
}

function fileSetting(file: string, rule: string): Setting | null {
  const text = readFileSync(file, "utf8");
  if (/\.ya?ml$/.test(file)) return yamlSetting(text, rule);

  const setting = parsedSetting(file, text, rule);
  return setting === "unknown" && file.endsWith(".eslintrc") ? yamlSetting(text, rule) : setting;
}

function directorySetting(dir: string, linter: Linter): Setting | null {
  for (const name of linter.files) {
    const file = join(dir, name);
    if (!existsSync(file)) continue;

    const setting = fileSetting(file, linter.rule);
    if (setting !== null) return setting;
  }

  return null;
}

function effectiveSetting(dir: string, linter: Linter): Setting | null {
  const own = directorySetting(dir, linter);
  if (own !== null) return own;

  const parent = dirname(dir);
  return parent === dir ? null : effectiveSetting(parent, linter);
}

export function bracesEnforced(dir: string): boolean {
  const cached = cache.get(dir);
  if (cached !== undefined) return cached;

  const result = LINTERS.some((linter) => {
    const setting = effectiveSetting(dir, linter);
    return setting !== null && setting !== "off";
  });

  cache.set(dir, result);
  return result;
}
