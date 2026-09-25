import { existsSync, realpathSync } from "node:fs";
import { basename, dirname, join, relative, resolve, sep } from "node:path";
import type { Node } from "oxc-parser";
import { walk } from "./ast.ts";
import { globReach } from "./config-glob.ts";
import {
  UNKNOWN,
  NO_SETTING,
  type Value,
  type ConfigModule,
  evaluate,
  exported,
  known,
  moduleAt,
  object,
  origin,
  property,
  resolveModule,
} from "./config-static.ts";

export type Setting = "on" | "off" | "unknown";
export type Reach = "all" | "none" | "some";

type Coverage = Reach | ((dir: string, extension?: string) => Reach);

export interface Layer {
  reach: Coverage;
  setting: Setting;
}

type Family = "flat" | "legacy" | "biome" | "oxlint";

interface Linter {
  family: Family;
  files: string[];
}

interface Context {
  family: Family;
  file: string;
  shared: boolean;
  chain: string[];
  consumed: Set<Node>;
  ancestors: Value[];
  modules: Set<ConfigModule>;
}

const LINTERS: Linter[] = [
  {
    family: "flat",
    files: ["js", "mjs", "cjs", "ts", "mts", "cts"].map((ext) => `eslint.config.${ext}`),
  },
  {
    family: "legacy",
    files: [
      ".eslintrc.js",
      ".eslintrc.cjs",
      ".eslintrc.yaml",
      ".eslintrc.yml",
      ".eslintrc.json",
      ".eslintrc",
    ],
  },
  { family: "biome", files: ["biome.json", "biome.jsonc"] },
  {
    family: "oxlint",
    files: [".oxlintrc.json", ".oxlintrc.jsonc", "oxlint.config.ts", "oxlint.config.mts"],
  },
];

const cache = new Map<string, boolean>();
const layerCache = new Map<string, { layers: Layer[]; root: Value }>();
const directoryFiles = new Map<string, string[]>();

function levelOf(value: Value): Setting {
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

function intersectCoverage(left: Coverage, right: Coverage): Coverage {
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

function read(value: Value, key: string, context: Context): Value {
  const node = origin(value, key);
  if (node) context.consumed.add(node);
  return property(value, key);
}

function patterns(
  value: Value,
  context: Context,
  mode: "anchored" | "legacy" | "biome1",
): Coverage {
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

function scope(value: Value, context: Context): Coverage {
  if (context.family === "flat" && property(value, "basePath") !== undefined) return "some";

  const biome = context.family === "biome";
  const includes = property(value, biome ? "includes" : "files");
  const mode = context.family === "legacy" || context.family === "oxlint" ? "legacy" : "anchored";

  let reach = patterns(includes, context, mode);
  if (biome && includes === undefined)
    reach = patterns(property(value, "include"), context, "biome1");

  const exclusion = property(
    value,
    biome
      ? "ignore"
      : context.family === "legacy"
        ? "excludedFiles"
        : context.family === "oxlint"
          ? "excludeFiles"
          : "ignores",
  );

  if (exclusion === undefined) return reach;

  const excluded = patterns(exclusion, context, mode);
  return (dir, extension) => {
    const included = reachAt(reach, dir, extension);
    return included === "all" && reachAt(excluded, dir, extension) !== "none" ? "some" : included;
  };
}

function oxlintLevel(value: Value): Setting {
  const seen = new Set<Value>();
  while (!seen.has(value)) {
    if (value === "off" || value === "allow" || value === 0) return "off";
    if (value === "warn" || value === "error" || value === "deny" || value === 1 || value === 2)
      return "on";
    if (typeof value === "string" || typeof value === "number") return "unknown";

    seen.add(value);

    if (Array.isArray(value)) value = value[0];
    else return "unknown";
  }

  return "unknown";
}

interface BiomeFindings {
  rule: Setting[];
  group: Setting[];
}

function biomeSettings(
  value: Value,
  context: Context,
  inside = "",
  found: BiomeFindings = { rule: [], group: [] },
): BiomeFindings {
  if (value === UNKNOWN) found.rule.push("unknown");
  else if (Array.isArray(value))
    for (const item of value) biomeSettings(item, context, inside, found);
  else if (object(value))
    for (const key of Object.keys(value)) {
      if (["extends", "overrides", "includes", "include", "ignore"].includes(key)) continue;

      const item = read(value, key, context);
      if (key === "useBlockStatements") found.rule.push(levelOf(item));
      else if (key === "style" && inside === "rules" && typeof item === "string" && item !== "off")
        found.group.push("on");
      else if (key === "all" && item === true && (inside === "rules" || inside === "style"))
        found.group.push("on");
      else biomeSettings(item, context, key, found);
    }

  return found;
}

function ownLayers(value: Value, context: Context): Layer[] {
  if (value === UNKNOWN) return [{ reach: "all", setting: "unknown" }];

  if (context.family === "biome") {
    const findings = biomeSettings(value, context);
    const found = findings.rule.length ? findings.rule : findings.group;
    return found.length
      ? [
          {
            reach: "all",
            setting: found.every((item) => item === "off")
              ? "off"
              : found.includes("unknown")
                ? "unknown"
                : "on",
          },
        ]
      : [];
  }

  if (context.family === "oxlint") return oxlintOwnLayers(value, context, "rules");

  const rules = property(value, "rules");
  const curly = read(rules, "curly", context);
  return curly === undefined || curly === NO_SETTING
    ? []
    : [{ reach: "all", setting: levelOf(curly) }];
}

type OxlintPhase = "categories" | "rules" | "overrides";

function oxlintOwnLayers(value: Value, context: Context, phase: OxlintPhase): Layer[] {
  if (value === UNKNOWN) return [{ reach: "all", setting: "unknown" }];

  if (phase === "categories") {
    const style = read(read(value, "categories", context), "style", context);
    return style === undefined || style === NO_SETTING
      ? []
      : [{ reach: "all", setting: oxlintLevel(style) }];
  }

  if (phase === "rules") {
    const rules = read(value, "rules", context);
    if (!object(rules))
      return rules === undefined || rules === NO_SETTING
        ? []
        : [{ reach: "all", setting: "unknown" }];

    let setting: Setting | undefined;
    for (const key of Object.keys(rules))
      if (key === "curly" || key === "eslint/curly")
        setting = oxlintLevel(read(rules, key, context));

    return setting === undefined ? [] : [{ reach: "all", setting }];
  }

  const overrides = property(value, "overrides");
  if (overrides === undefined) return [];
  if (!Array.isArray(overrides)) return [{ reach: "all", setting: "unknown" }];

  return overrides.flatMap((item) => {
    const reach = scope(item, context);
    return oxlintOwnLayers(item, context, "rules").map((layer) => ({
      ...layer,
      reach: intersectCoverage(layer.reach, reach),
    }));
  });
}

function packageName(name: string): string {
  if (name.startsWith("@")) {
    const [scope, pkg] = name.split("/");
    return `${scope}/${pkg?.startsWith("eslint-config") ? pkg : pkg ? `eslint-config-${pkg}` : "eslint-config"}`;
  }

  return name.startsWith("eslint-config-") ? name : `eslint-config-${name}`;
}

function extend(entry: Value, parent: Value, context: Context): Layer[] {
  if (typeof entry !== "string") return layers(entry, { ...context, shared: true });
  if (entry === "//" && context.family === "biome") {
    let dir = dirname(context.file);
    while (dirname(dir) !== dir) {
      dir = dirname(dir);

      for (const name of ["biome.json", "biome.jsonc"]) {
        const file = join(dir, name);
        if (existsSync(file)) return fileLayers(file, { ...context, shared: false }).layers;
      }
    }

    return [{ reach: "all", setting: "unknown" }];
  }

  const preset = known(entry);
  if (preset !== undefined) return layers(preset, { ...context, shared: true });

  if (context.family === "flat" && !entry.startsWith(".") && !entry.startsWith("/")) {
    const slash = entry.lastIndexOf("/");
    const plugin = property(property(parent, "plugins"), entry.slice(0, slash));
    if (slash > 0 && plugin !== undefined)
      return layers(property(property(plugin, "configs"), entry.slice(slash + 1)), {
        ...context,
        shared: true,
      });
  }

  const specifier =
    context.family === "legacy" && !entry.startsWith(".") && !entry.startsWith("/")
      ? packageName(entry)
      : entry;

  const file = resolveModule(
    specifier,
    context.file,
    context.family === "legacy" ? "require" : "import",
  );

  return file
    ? fileLayers(file, { ...context, shared: true }).layers
    : [{ reach: "all", setting: "unknown" }];
}

interface OxlintValue {
  value: Value;
  context: Context;
}

function oxlintExtends(value: Value, context: Context): OxlintValue[] | null {
  const inherited = property(value, "extends");
  if (inherited === undefined) return [];
  if (!Array.isArray(inherited)) return null;

  const result: OxlintValue[] = [];
  for (const entry of inherited) {
    if (typeof entry !== "string") {
      result.push({ value: entry, context: { ...context, shared: true } });
      continue;
    }

    if (!entry.startsWith(".")) return null;

    const file = resolveModule(entry, context.file, "import");
    const module = file && moduleAt(file);
    if (!module || context.chain.includes(module.file)) return null;

    context.modules.add(module);

    result.push({
      value: exported(module.file, "default", context.chain),
      context: {
        ...context,
        file: module.file,
        shared: true,
        chain: [...context.chain, module.file],
      },
    });
  }

  return result;
}

function oxlintLayers(value: Value, context: Context): Layer[] {
  return (["categories", "rules", "overrides"] as const).flatMap((phase) =>
    oxlintPhaseLayers(value, context, phase),
  );
}

function oxlintPhaseLayers(value: Value, context: Context, phase: OxlintPhase): Layer[] {
  if (value === NO_SETTING) return [];
  if (value === UNKNOWN || !object(value) || context.ancestors.includes(value))
    return [{ reach: "all", setting: "unknown" }];

  const next = { ...context, ancestors: [...context.ancestors, value] };
  const entries = oxlintExtends(value, next);
  if (!entries) return [{ reach: "all", setting: "unknown" }];

  return [
    ...entries.flatMap((entry) => oxlintPhaseLayers(entry.value, entry.context, phase)),
    ...oxlintOwnLayers(value, next, phase),
  ];
}

function layers(value: Value, context: Context): Layer[] {
  if (context.family === "oxlint") return oxlintLayers(value, context);
  if (value === NO_SETTING) return [];
  if (context.ancestors.includes(value)) return [{ reach: "all", setting: "unknown" }];

  context = { ...context, ancestors: [...context.ancestors, value] };
  if (Array.isArray(value)) return value.flatMap((item) => layers(item, context));
  if (!object(value)) return [{ reach: "all", setting: "unknown" }];

  const reach = scope(value, context);
  const inherited = property(value, "extends");
  const entries: Value[] =
    inherited === undefined ? [] : Array.isArray(inherited) ? inherited : [inherited];

  const result = entries.flatMap((entry) => extend(entry, value, context));
  result.push(...ownLayers(value, context));

  const scoped = result.map((layer) => ({
    ...layer,
    reach: intersectCoverage(layer.reach, reach),
  }));

  const overrides = property(value, "overrides");
  if (overrides !== undefined) {
    const items: Value[] = Array.isArray(overrides) ? overrides : [UNKNOWN];
    for (const item of items)
      scoped.push(
        ...layers(item, context).map((layer) => ({
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

  const rules =
    context.family === "biome"
      ? ["useBlockStatements"]
      : context.family === "oxlint"
        ? ["curly", "eslint/curly", "style"]
        : ["curly"];

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
      rules.includes(key) &&
      levelOf(evaluate(node.value, module, context.chain)) !== "off"
    )
      result.push({ reach: "all", setting: "unknown" });
  });
}

function fileLayers(file: string, context: Context): { layers: Layer[]; root: Value } {
  const module = moduleAt(file);
  if (!module || context.chain.includes(module.file) || context.chain.length >= 32)
    return { layers: [{ reach: "all", setting: "unknown" }], root: undefined };

  const key = JSON.stringify([file, dirname(file), context.family, context.shared]);
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
  const result = layers(value, next);
  if (context.family === "oxlint")
    for (const candidate of next.modules)
      backstop(candidate, { ...next, file: candidate.file, chain: [] }, result);
  else backstop(module, next, result);

  return { layers: result, root: property(value, "root") };
}

function effectiveSetting(
  dir: string,
  extension: string | undefined,
  linter: Linter,
): Setting | null {
  const found: { setting: Setting | null; root: Value }[] = [];

  let current = dir;
  while (true) {
    const key = `${linter.family}:${current}`;

    let files = directoryFiles.get(key);
    if (!files) {
      files = linter.files.map((name) => join(current, name)).filter((file) => existsSync(file));
      directoryFiles.set(key, files);
    }

    if (linter.family === "oxlint" && files.length > 1) return "unknown";

    for (const file of files) {
      const result = fileLayers(file, {
        family: linter.family,
        file,
        shared: false,
        chain: [],
        consumed: new Set(),
        ancestors: [],
        modules: new Set(),
      });

      const setting = fold(result.layers, dir, extension);
      if (linter.family === "oxlint") return setting;
      if (linter.family === "legacy") {
        if (setting !== null || result.root === true) return setting;
        break;
      }

      found.push({ setting, root: result.root });
    }

    const parent = dirname(current);
    if (parent === current) break;

    current = parent;
  }

  const nearest = found.find((item) => item.setting !== null);
  if (linter.family === "biome" && nearest?.root === false) return nearest.setting;

  // ESLint 9 uses the cwd config; ESLint 10 uses the nearest config.
  return (
    found.find((item) => item.setting === "on" || item.setting === "unknown")?.setting ??
    nearest?.setting ??
    null
  );
}

function realDirectory(dir: string): string {
  if (existsSync(dir)) return realpathSync(dir);
  const parent = dirname(dir);
  return parent === dir ? dir : join(realDirectory(parent), basename(dir));
}

export function bracesEnforced(dir: string, extension?: string): boolean {
  const requested = `${dir}\0${extension ?? ""}`;
  const cached = cache.get(requested);
  if (cached !== undefined) return cached;

  let result = true;
  try {
    dir = realDirectory(resolve(dir));
    result = LINTERS.some((linter) => {
      const setting = effectiveSetting(dir, extension, linter);
      return setting !== null && setting !== "off";
    });
  } catch {}

  cache.set(requested, result);
  return result;
}
