import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { UNKNOWN, type Value, object, property } from "./evaluate.ts";
import {
  type Context,
  type Layer,
  type Reader,
  type Tree,
  type Setting,
  UNKNOWN_LAYER,
  decided,
  enforcing,
  everyConfig,
  excluding,
  fileLayers,
  levelOf,
  patterns,
  read,
  resolved,
  tree,
} from "./layers.ts";

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

const biomeTree: Tree = {
  scope(value, context) {
    const includes = property(value, "includes");

    let reach = patterns(includes, context, "anchored");
    if (includes === undefined) reach = patterns(property(value, "include"), context, "biome1");

    return excluding(reach, property(value, "ignore"), context, "anchored");
  },
  own: ownLayers,
  extend(entry, _parent, context) {
    if (entry === "//") {
      let dir = dirname(context.file);
      while (dirname(dir) !== dir) {
        dir = dirname(dir);

        for (const name of biome.files) {
          const file = join(dir, name);
          if (existsSync(file)) return fileLayers(file, { ...context, shared: false }).layers;
        }
      }

      return [UNKNOWN_LAYER];
    }

    return resolved(entry, context, "import");
  },
};

export const biome: Reader = {
  family: "biome",
  files: ["biome.json", "biome.jsonc"],
  backstop: ["useBlockStatements"],
  backstopImports: false,
  layers(value, context) {
    return tree(value, context, biomeTree);
  },
  decide(dir, extension) {
    const found = everyConfig(dir, extension, biome);
    const nearest = found.find((item) => item.setting !== null);
    return decided(biome, nearest?.root === false ? nearest : enforcing(found));
  },
};
