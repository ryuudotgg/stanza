import { UNKNOWN, NO_SETTING, type Value, exported, object, property } from "./evaluate.ts";
import { configDirectories, resolveModule } from "./find.ts";
import { moduleAt } from "./module.ts";
import {
  type Context,
  type Coverage,
  type Layer,
  type Reader,
  type Setting,
  UNKNOWN_LAYER,
  configAt,
  decided,
  excluding,
  intersectCoverage,
  patterns,
  read,
} from "./layers.ts";

function scope(value: Value, context: Context): Coverage {
  const reach = patterns(property(value, "files"), context, "legacy");
  return excluding(reach, property(value, "excludeFiles"), context, "legacy");
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

type OxlintPhase = "categories" | "rules" | "overrides";

function oxlintOwnLayers(value: Value, context: Context, phase: OxlintPhase): Layer[] {
  if (value === UNKNOWN) return [UNKNOWN_LAYER];

  if (phase === "categories") {
    const style = read(read(value, "categories", context), "style", context);
    return style === undefined || style === NO_SETTING
      ? []
      : [{ reach: "all", setting: oxlintLevel(style) }];
  }

  if (phase === "rules") {
    const rules = read(value, "rules", context);
    if (!object(rules)) return rules === undefined || rules === NO_SETTING ? [] : [UNKNOWN_LAYER];

    let setting: Setting | undefined;
    for (const key of Object.keys(rules))
      if (key === "curly" || key === "eslint/curly")
        setting = oxlintLevel(read(rules, key, context));

    return setting === undefined ? [] : [{ reach: "all", setting }];
  }

  const overrides = property(value, "overrides");
  if (overrides === undefined) return [];
  if (!Array.isArray(overrides)) return [UNKNOWN_LAYER];

  return overrides.flatMap((item) => {
    const reach = scope(item, context);
    return oxlintOwnLayers(item, context, "rules").map((layer) => ({
      ...layer,
      reach: intersectCoverage(layer.reach, reach),
    }));
  });
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
    return [UNKNOWN_LAYER];

  const next = { ...context, ancestors: [...context.ancestors, value] };
  const entries = oxlintExtends(value, next);
  if (!entries) return [UNKNOWN_LAYER];

  return [
    ...entries.flatMap((entry) => oxlintPhaseLayers(entry.value, entry.context, phase)),
    ...oxlintOwnLayers(value, next, phase),
  ];
}

export const oxlint: Reader = {
  family: "oxlint",
  files: [".oxlintrc.json", ".oxlintrc.jsonc", "oxlint.config.ts", "oxlint.config.mts"],
  backstop: ["curly", "eslint/curly", "style"],
  backstopImports: true,
  layers: oxlintLayers,
  decide(dir, extension) {
    for (const files of configDirectories(dir, oxlint)) {
      if (files.length > 1) return { family: oxlint.family, setting: "unknown", files };

      const file = files[0];
      if (file === undefined) continue;

      return decided(oxlint, configAt(file, dir, extension, oxlint));
    }

    return null;
  },
};
