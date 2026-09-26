import { NO_SETTING, type Value, property } from "./evaluate.ts";
import { configDirectories } from "./find.ts";
import {
  type Context,
  type Layer,
  type Reader,
  type Tree,
  configAt,
  decided,
  enforcing,
  everyConfig,
  excluding,
  levelOf,
  patterns,
  read,
  resolved,
  tree,
} from "./layers.ts";

function ownLayers(value: Value, context: Context): Layer[] {
  const rules = property(value, "rules");
  const curly = read(rules, "curly", context);
  return curly === undefined || curly === NO_SETTING
    ? []
    : [{ reach: "all", setting: levelOf(curly) }];
}

function packageName(name: string): string {
  if (name.startsWith("@")) {
    const [scope, pkg] = name.split("/");
    return `${scope}/${pkg?.startsWith("eslint-config") ? pkg : pkg ? `eslint-config-${pkg}` : "eslint-config"}`;
  }

  return name.startsWith("eslint-config-") ? name : `eslint-config-${name}`;
}

const flatTree: Tree = {
  scope(value, context) {
    if (property(value, "basePath") !== undefined) return "some";
    const reach = patterns(property(value, "files"), context, "anchored");
    return excluding(reach, property(value, "ignores"), context, "anchored");
  },
  own: ownLayers,
  extend(entry, parent, context) {
    if (!entry.startsWith(".") && !entry.startsWith("/")) {
      const slash = entry.lastIndexOf("/");
      const plugin = property(property(parent, "plugins"), entry.slice(0, slash));
      if (slash > 0 && plugin !== undefined)
        return context.reader.layers(
          property(property(plugin, "configs"), entry.slice(slash + 1)),
          {
            ...context,
            shared: true,
          },
        );
    }

    return resolved(entry, context, "import");
  },
};

const legacyTree: Tree = {
  scope(value, context) {
    const reach = patterns(property(value, "files"), context, "legacy");
    return excluding(reach, property(value, "excludedFiles"), context, "legacy");
  },
  own: ownLayers,
  extend(entry, _parent, context) {
    const specifier = !entry.startsWith(".") && !entry.startsWith("/") ? packageName(entry) : entry;
    return resolved(specifier, context, "require");
  },
};

export const flat: Reader = {
  family: "flat",
  files: ["js", "mjs", "cjs", "ts", "mts", "cts"].map((ext) => `eslint.config.${ext}`),
  backstop: ["curly"],
  backstopImports: false,
  layers(value, context) {
    return tree(value, context, flatTree);
  },
  decide(dir, extension) {
    return decided(flat, enforcing(everyConfig(dir, extension, flat)));
  },
};

export const legacy: Reader = {
  family: "legacy",
  files: [
    ".eslintrc.js",
    ".eslintrc.cjs",
    ".eslintrc.yaml",
    ".eslintrc.yml",
    ".eslintrc.json",
    ".eslintrc",
  ],
  backstop: ["curly"],
  backstopImports: false,
  layers(value, context) {
    return tree(value, context, legacyTree);
  },
  decide(dir, extension) {
    for (const files of configDirectories(dir, legacy)) {
      const file = files[0];
      if (file === undefined) continue;

      const found = configAt(file, dir, extension, legacy);
      if (found.setting !== null || found.root === true) return decided(legacy, found);
    }

    return null;
  },
};
