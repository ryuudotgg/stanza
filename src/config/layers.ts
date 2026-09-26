import { dirname, relative, sep } from "node:path";
import type { Node } from "oxc-parser";
import { walk } from "../ast.ts";
import {
  UNKNOWN,
  NO_SETTING,
  type Value,
  evaluate,
  exported,
  known,
  object,
  origin,
  property,
} from "./evaluate.ts";
import { configDirectories, resolveModule, type Loader } from "./find.ts";
import { globReach } from "./glob.ts";
import { moduleAt, type ConfigModule } from "./module.ts";

export type Setting = "on" | "off" | "unknown";
export type Reach = "all" | "none" | "some";
export type Coverage = Reach | ((dir: string, extension?: string) => Reach);
export type Family = "flat" | "legacy" | "biome" | "oxlint";

export interface Layer {
  readonly reach: Coverage;
  readonly setting: Setting;
}

export interface Decision {
  family: Family;
  setting: Setting;
  files: string[];
}

export interface Reader {
  family: Family;
  files: readonly string[];
  backstop: readonly string[];
  backstopImports: boolean;
  layers(value: Value, context: Context): Layer[];
  decide(dir: string, extension: string | undefined): Decision | null;
}

export interface Tree {
  scope(value: Value, context: Context): Coverage;
  own(value: Value, context: Context): Layer[];
  extend(entry: string, parent: Value, context: Context): Layer[];
}

export type Mode = "anchored" | "legacy" | "biome1";

export interface Found {
  setting: Setting | null;
  root: Value;
  file: string;
}

export interface Context {
  reader: Reader;
  file: string;
  shared: boolean;
  chain: string[];
  consumed: Set<Node>;
  ancestors: Value[];
  modules: Set<ConfigModule>;
}

export const UNKNOWN_LAYER: Layer = { reach: "all", setting: "unknown" };

const layerCache = new Map<string, { layers: Layer[]; root: Value }>();

export function levelOf(value: Value): Setting {
  const seen = new Set<Value>();
  while (!seen.has(value)) {
    if (value === "off" || value === 0) return "off";
    if (typeof value === "string" || typeof value === "number") return "on";

    seen.add(value);

    if (Array.isArray(value)) value = value[0];
    else if (object(value)) value = property(value, "level");
    else return "unknown";
  }

  return "unknown";
}

function intersect(left: Reach, right: Reach): Reach {
  if (left === "none" || right === "none") return "none";
  return left === "all" ? right : right === "all" ? left : "some";
}

function reachAt(coverage: Coverage, dir: string, extension?: string): Reach {
  return typeof coverage === "function" ? coverage(dir, extension) : coverage;
}

export function intersectCoverage(left: Coverage, right: Coverage): Coverage {
  if (left === "all") return right;
  if (right === "all") return left;
  if (left === "none" || right === "none") return "none";
  return (dir, extension) =>
    intersect(reachAt(left, dir, extension), reachAt(right, dir, extension));
}

function fold(layers: Layer[], dir: string, extension?: string): Setting | null {
  let setting: Setting | null = null;
  for (const layer of layers) {
    const reach = reachAt(layer.reach, dir, extension);
    if (reach === "all" || (reach === "some" && layer.setting !== "off")) setting = layer.setting;
  }

  return setting;
}

export function read(value: Value, key: string, context: Context): Value {
  const node = origin(value, key);
  if (node) context.consumed.add(node);
  return property(value, key);
}

export function patterns(value: Value, context: Context, mode: Mode): Coverage {
  if (value === undefined) return "all";
  if (!Array.isArray(value) || !value.every((item): item is string => typeof item === "string"))
    return "some";
  if (context.shared) return "some";
  if (value.length === 0) return "none";
  if (mode === "biome1") return "some";

  const anchor = dirname(context.file);
  return (dir, extension) =>
    globReach(value, relative(anchor, dir).split(sep).join("/"), mode === "legacy", extension);
}

export function excluding(
  reach: Coverage,
  exclusion: Value,
  context: Context,
  mode: Mode,
): Coverage {
  if (exclusion === undefined) return reach;

  const excluded = patterns(exclusion, context, mode);
  return (dir, extension) => {
    const included = reachAt(reach, dir, extension);
    return included === "all" && reachAt(excluded, dir, extension) !== "none" ? "some" : included;
  };
}

function inherit(entry: Value, parent: Value, context: Context, hooks: Tree): Layer[] {
  if (typeof entry !== "string") return context.reader.layers(entry, { ...context, shared: true });

  const preset = known(entry);
  if (preset !== undefined) return context.reader.layers(preset, { ...context, shared: true });

  return hooks.extend(entry, parent, context);
}

export function resolved(specifier: string, context: Context, loader: Loader): Layer[] {
  const file = resolveModule(specifier, context.file, loader);
  return file ? fileLayers(file, { ...context, shared: true }).layers : [UNKNOWN_LAYER];
}

export function tree(value: Value, context: Context, hooks: Tree): Layer[] {
  if (value === NO_SETTING) return [];
  if (context.ancestors.includes(value)) return [UNKNOWN_LAYER];

  context = { ...context, ancestors: [...context.ancestors, value] };
  if (Array.isArray(value)) return value.flatMap((item) => tree(item, context, hooks));
  if (!object(value)) return [UNKNOWN_LAYER];

  const reach = hooks.scope(value, context);
  const inherited = property(value, "extends");
  const entries: Value[] =
    inherited === undefined ? [] : Array.isArray(inherited) ? inherited : [inherited];

  const result = entries.flatMap((entry) => inherit(entry, value, context, hooks));
  result.push(...hooks.own(value, context));

  const scoped = result.map((layer) => ({
    ...layer,
    reach: intersectCoverage(layer.reach, reach),
  }));

  const overrides = property(value, "overrides");
  if (overrides !== undefined) {
    const items: Value[] = Array.isArray(overrides) ? overrides : [UNKNOWN];
    for (const item of items)
      scoped.push(
        ...tree(item, context, hooks).map((layer) => ({
          ...layer,
          reach: intersectCoverage(reach, layer.reach),
        })),
      );
  }

  return scoped;
}

function yamlSetting(text: string): Setting | null {
  const lines = text.split(/\r?\n/).map((line) => line.replace(/\s#.*$/, "").trimEnd());
  const index = lines.findIndex((line) => /^\s*["']?curly["']?\s*:/.test(line));

  if (lines.some((line, at) => at !== index && /\bcurly\b/.test(line))) return "unknown";

  let setting: Setting | null = null;
  if (index !== -1) {
    let value = lines[index]!.replace(/^[^:]*:\s*/, "");
    if (value === "")
      value =
        lines
          .slice(index + 1)
          .find((line) => line.trim() !== "")
          ?.trim() ?? "";

    const first = value
      .replace(/[[\]]/g, "")
      .replace(/^[-\s]+/, "")
      .split(",")[0]!
      .trim()
      .replace(/^["']|["']$/g, "");

    setting = first === "off" || first === "0" ? "off" : "on";
  }

  return setting !== "on" && lines.some((line) => /^(extends|overrides)\s*:/.test(line))
    ? "unknown"
    : setting;
}

function backstop(module: ConfigModule, context: Context, result: Layer[]): void {
  if (!module.program) return;

  walk(module.program, (node) => {
    if (node.type !== "Property" || context.consumed.has(node)) return;

    const key =
      node.key.type === "Identifier"
        ? node.key.name
        : node.key.type === "Literal"
          ? node.key.value
          : null;

    if (
      typeof key === "string" &&
      context.reader.backstop.includes(key) &&
      levelOf(evaluate(node.value, module, context.chain)) !== "off"
    )
      result.push(UNKNOWN_LAYER);
  });
}

export function fileLayers(file: string, context: Context): { layers: Layer[]; root: Value } {
  const module = moduleAt(file);
  if (!module || context.chain.includes(module.file) || context.chain.length >= 32)
    return { layers: [UNKNOWN_LAYER], root: undefined };

  const key = JSON.stringify([file, dirname(file), context.reader.family, context.shared]);
  const cached = layerCache.get(key);
  if (cached) return cached;

  const result = readFileLayers(file, module, context);
  layerCache.set(key, result);
  return result;
}

function readFileLayers(
  file: string,
  module: ConfigModule,
  context: Context,
): { layers: Layer[]; root: Value } {
  const next = { ...context, file, chain: [...context.chain, module.file] };
  next.modules.add(module);

  if (module.yaml) {
    const setting = yamlSetting(module.text);
    return {
      layers: setting === null ? [] : [{ reach: "all", setting }],
      root: /^root:\s*true\s*$/m.test(module.text),
    };
  }

  const value = exported(module.file, "default", context.chain);
  const result = context.reader.layers(value, next);
  if (context.reader.backstopImports)
    for (const candidate of next.modules)
      backstop(candidate, { ...next, file: candidate.file, chain: [] }, result);
  else backstop(module, next, result);

  return { layers: result, root: property(value, "root") };
}

export function configAt(
  file: string,
  dir: string,
  extension: string | undefined,
  reader: Reader,
): Found {
  const result = fileLayers(file, {
    reader,
    file,
    shared: false,
    chain: [],
    consumed: new Set(),
    ancestors: [],
    modules: new Set(),
  });

  return { setting: fold(result.layers, dir, extension), root: result.root, file };
}

export function decided(reader: Reader, found: Found | null): Decision | null {
  return found && found.setting !== null
    ? { family: reader.family, setting: found.setting, files: [found.file] }
    : null;
}

export function everyConfig(dir: string, extension: string | undefined, reader: Reader): Found[] {
  return [...configDirectories(dir, reader)].flatMap((files) =>
    files.map((file) => configAt(file, dir, extension, reader)),
  );
}

// ESLint 9 uses the cwd config; ESLint 10 uses the nearest config.
export function enforcing(found: Found[]): Found | null {
  const nearest = found.find((item) => item.setting !== null);
  return (
    found.find((item) => item.setting === "on" || item.setting === "unknown") ?? nearest ?? null
  );
}
