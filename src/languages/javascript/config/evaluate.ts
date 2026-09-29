import type { Node } from "oxc-parser";
import { children } from "../ast.ts";
import { resolveModule } from "./find.ts";
import {
  type Binding,
  type ConfigModule,
  helper,
  memberName,
  moduleAt,
  nameOf,
  requiredSource,
} from "./module.ts";

export const UNKNOWN = Symbol("unknown");
export const NO_SETTING = Symbol("no setting");
export type Value =
  | string
  | number
  | boolean
  | null
  | undefined
  | typeof UNKNOWN
  | typeof NO_SETTING
  | Value[]
  | { [key: string]: Value };

const emptyConfigs: Value = new Proxy({}, { get: () => [] });
const js: Value = { configs: { recommended: { rules: {} }, all: { rules: { curly: "error" } } } };
const prettier: Value = { rules: { curly: "off" } };
const KNOWN: { pattern: RegExp; value: Value }[] = [
  { pattern: /^eslint:recommended$/, value: {} },
  { pattern: /^eslint:all$/, value: { rules: { curly: "error" } } },
  { pattern: /^@eslint\/js$/, value: js },
  {
    pattern: /^typescript-eslint$/,
    value: { configs: emptyConfigs },
  },
  {
    pattern:
      /^(prettier|eslint-config-prettier(?:\/flat)?|plugin:prettier\/recommended|eslint-plugin-prettier\/recommended)$/,
    value: prettier,
  },
  { pattern: /^eslint\/config$/, value: {} },
];

const origins = new WeakMap<object, Map<string, Node>>();

export function object(value: Value): value is { [key: string]: Value } {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function property(value: Value, key: string): Value {
  if (value === undefined || value === NO_SETTING) return value;
  return object(value) ? value[key] : UNKNOWN;
}

export function origin(value: Value, key: string): Node | undefined {
  return object(value) ? origins.get(value)?.get(key) : undefined;
}

export function known(name: string): Value {
  const preset = KNOWN.find((entry) => entry.pattern.test(name));
  if (preset) return preset.value;

  const parts = name.split("/");
  const packageName = parts.slice(0, name.startsWith("@") ? 2 : 1).join("/");
  return name.startsWith("plugin:") ||
    /^eslint-plugin-|^@[^/]+\/eslint-plugin(-.*)?$/.test(packageName)
    ? NO_SETTING
    : undefined;
}

function bindingValue(binding: Binding | undefined, module: ConfigModule, chain: string[]): Value {
  if (!binding || binding.mutable) return UNKNOWN;
  if (binding.resolved) return binding.value;

  binding.resolved = true;
  binding.value = UNKNOWN;

  let value: Value;
  if (binding.imported) {
    const preset = known(binding.imported.source);
    const file =
      preset === undefined
        ? resolveModule(binding.imported.source, module.file, binding.imported.loader)
        : undefined;

    value =
      preset !== undefined
        ? binding.imported.name === "default" || binding.imported.name === "*"
          ? preset
          : property(preset, binding.imported.name)
        : file
          ? exported(file, binding.imported.name, chain)
          : UNKNOWN;
  } else if (binding.local) value = bindingValue(module.bindings.get(binding.local), module, chain);
  else value = binding.node ? evaluate(binding.node, module, chain) : UNKNOWN;

  binding.value = value;
  return value;
}

export function exported(file: string, name: string, chain: string[] = []): Value {
  const module = moduleAt(file);
  if (
    !module ||
    !module.program ||
    module.mutated ||
    chain.includes(module.file) ||
    chain.length >= 32
  )
    return UNKNOWN;

  if (module.values.has(name)) return module.values.get(name);

  const next = [...chain, module.file];

  let value: Value;
  if (name === "*") {
    const namespace: { [key: string]: Value } = {};
    for (const key of module.exports.keys())
      Object.defineProperty(namespace, key, {
        enumerable: true,
        configurable: true,
        get: () => exported(file, key, chain),
      });

    value = namespace;
  } else value = bindingValue(module.exports.get(name), module, next);

  module.values.set(name, value);
  return value;
}

function objectValue(
  node: Extract<Node, { type: "ObjectExpression" }>,
  module: ConfigModule,
  chain: string[],
): Value {
  const result: { [key: string]: Value } = Object.create(null);
  const locations = new Map<string, Node>();
  for (const item of node.properties)
    if (item.type === "SpreadElement") {
      const value = evaluate(item.argument, module, chain);
      if (value === NO_SETTING) continue;
      if (!object(value)) return UNKNOWN;

      for (const key of Object.keys(value))
        Object.defineProperty(result, key, {
          enumerable: true,
          configurable: true,
          get: () => value[key],
        });

      for (const [key, location] of origins.get(value) ?? []) locations.set(key, location);
    } else {
      const key = !item.computed && nameOf(item.key);
      if (!key) return UNKNOWN;

      let value: Value;
      let resolved = false;
      Object.defineProperty(result, key, {
        enumerable: true,
        configurable: true,
        get: () => {
          if (!resolved) {
            resolved = true;
            value = UNKNOWN;
            value =
              item.method || item.kind !== "init" ? UNKNOWN : evaluate(item.value, module, chain);
          }

          return value;
        },
      });

      locations.set(key, item);
    }

  origins.set(result, locations);
  return result;
}

export function evaluate(node: Node, module: ConfigModule, chain: string[]): Value {
  switch (node.type) {
    case "Literal":
      return typeof node.value === "bigint" ||
        (typeof node.value === "object" && node.value !== null)
        ? UNKNOWN
        : node.value;

    case "TemplateLiteral":
      return node.expressions.length === 0 ? node.quasis[0]?.value.cooked : UNKNOWN;

    case "ObjectExpression":
      return objectValue(node, module, chain);

    case "ArrayExpression": {
      if (node.elements.length > 10000) return UNKNOWN;

      const result: Value[] = [];
      for (const item of node.elements) {
        const value = item
          ? evaluate(item.type === "SpreadElement" ? item.argument : item, module, chain)
          : UNKNOWN;

        if (item?.type === "SpreadElement") {
          if (value === NO_SETTING) continue;
          if (!Array.isArray(value)) return UNKNOWN;
          if (result.length + value.length > 10000) return UNKNOWN;

          result.push(...value);
        } else result.push(value);

        if (result.length > 10000) return UNKNOWN;
      }

      return result;
    }

    case "Identifier":
      return bindingValue(module.bindings.get(node.name), module, chain);

    case "MemberExpression": {
      const value = evaluate(node.object, module, chain);
      if (value === NO_SETTING) return value;

      const key = memberName(node);
      return key === undefined ? UNKNOWN : property(value, key);
    }

    case "CallExpression": {
      const name = requiredSource(node);
      if (name !== undefined) {
        const preset = known(name);
        const file = preset === undefined ? resolveModule(name, module.file, "require") : undefined;
        return preset !== undefined ? preset : file ? exported(file, "default", chain) : UNKNOWN;
      }

      const kind = helper(node.callee, module);
      if (kind === "ignore") return {};
      if (kind === "identity")
        return node.arguments.length === 1 ? evaluate(node.arguments[0]!, module, chain) : UNKNOWN;

      if (kind === "flatten") {
        const result = node.arguments.flatMap((item): Value[] => {
          const value = evaluate(
            item.type === "SpreadElement" ? item.argument : item,
            module,
            chain,
          );

          return Array.isArray(value) ? value : [value];
        });

        return result.length > 10000 ? UNKNOWN : result;
      }

      return UNKNOWN;
    }

    default:
      if (
        [
          "TSAsExpression",
          "TSSatisfiesExpression",
          "TSNonNullExpression",
          "TSTypeAssertion",
          "ParenthesizedExpression",
        ].includes(node.type)
      ) {
        const expression = children(node).find(([key]) => key === "expression")?.[1];
        return expression ? evaluate(expression, module, chain) : UNKNOWN;
      }

      return UNKNOWN;
  }
}
