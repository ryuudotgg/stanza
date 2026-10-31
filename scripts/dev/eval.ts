import { readdirSync, realpathSync } from "node:fs";
import { join, relative, resolve } from "node:path";
import { walk } from "../../src/languages/javascript/ast.ts";
import { flat, legacy } from "../../src/languages/javascript/config/eslint.ts";
import { biome } from "../../src/languages/javascript/config/biome.ts";
import {
  exported,
  NO_SETTING,
  object,
  origin,
  UNKNOWN,
  type Value,
} from "../../src/languages/javascript/config/evaluate.ts";
import { loadedModules } from "../../src/languages/javascript/config/module.ts";
import { oxlint } from "../../src/languages/javascript/config/oxlint.ts";
import { FILES as FORMATTER_FILES } from "../../src/languages/javascript/config/width.ts";

type Node = NonNullable<ReturnType<typeof origin>>;

const NAMES = new Set([
  ...flat.files,
  ...legacy.files,
  ...biome.files,
  ...oxlint.files,
  ...FORMATTER_FILES,
]);

const SKIPPED = new Set([".git", "node_modules"]);
export function inputs(dir: string): string[] {
  const found: string[] = [];
  const pending = [""];
  while (pending.length > 0) {
    const current = pending.pop()!;
    for (const entry of readdirSync(join(dir, current), { withFileTypes: true })) {
      const path = current ? `${current}/${entry.name}` : entry.name;
      if (entry.isDirectory()) {
        if (!SKIPPED.has(entry.name)) pending.push(path);
      } else if (NAMES.has(entry.name)) found.push(path);
    }
  }

  return found.sort();
}

const cwd = realpathSync(process.cwd());
const nodeFiles = new WeakMap<Node, string>();
const indexed = new Set<string>();
function fileOf(node: Node): string {
  if (!nodeFiles.has(node))
    for (const module of loadedModules()) {
      if (!module.program || indexed.has(module.file)) continue;
      indexed.add(module.file);
      walk(module.program, (child) => nodeFiles.set(child, module.file));
    }

  return relative(cwd, nodeFiles.get(node) ?? "?");
}

function indented(items: string[], depth: number, open: string, close: string): string {
  if (items.length === 0) return `${open}${close}`;
  const inner = "  ".repeat(depth + 1);
  return `${open}\n${items.map((item) => `${inner}${item}`).join(",\n")}\n${"  ".repeat(depth)}${close}`;
}

function marker(name: string): string {
  return `{"$${name}": true}`;
}

function printed(
  value: Value,
  depth: number,
  path: string[],
  seen: Map<object, string>,
  origins: string[],
): string {
  if (value === undefined) return marker("undefined");
  if (value === UNKNOWN) return marker("unknown");
  if (value === NO_SETTING) return marker("noSetting");
  if (typeof value === "number" && !Number.isFinite(value)) return `{"$number": "${value}"}`;
  if (typeof value !== "object" || value === null) return JSON.stringify(value);

  const first = seen.get(value);
  if (first !== undefined) return `{"$ref": ${JSON.stringify(first)}}`;

  seen.set(value, path.join("."));

  let result: string;
  if (Array.isArray(value)) {
    const items = value.map((item, index) =>
      printed(item, depth + 1, [...path, String(index)], seen, origins),
    );

    result = indented(items, depth, "[", "]");
  } else {
    const items: string[] = [];
    for (const key of Object.keys(value)) {
      const node = object(value) ? origin(value, key) : undefined;
      const at = [...path, key].join(".");
      if (node)
        origins.push(`${JSON.stringify(at)}: ${JSON.stringify(`${fileOf(node)}:${node.start}`)}`);

      items.push(
        `${JSON.stringify(key)}: ${printed(value[key], depth + 1, [...path, key], seen, origins)}`,
      );
    }

    result = indented(items, depth, "{", "}");
  }

  return result;
}

function dump(file: string): string {
  const origins: string[] = [];
  const value = printed(exported(resolve(file), "default"), 2, [], new Map(), origins);
  const entry = [`"value": ${value}`, `"origins": ${indented(origins, 2, "{", "}")}`];
  return `${JSON.stringify(file)}: ${indented(entry, 1, "{", "}")}`;
}

if (import.meta.main) {
  const files = process.argv.slice(2);
  console.log(indented(files.map(dump), 0, "{", "}"));
}
