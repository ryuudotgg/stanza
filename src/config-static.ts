import { readFileSync, realpathSync, statSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import type { Node, Program } from "oxc-parser";
import { children, walk } from "./ast.ts";
import { parse } from "./parse.ts";

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

interface Binding {
  node?: Node;
  imported?: { source: string; name: string; loader: Loader };
  local?: string;
  mutable?: boolean;
  value?: Value;
  resolved?: boolean;
}

export interface ConfigModule {
  file: string;
  text: string;
  program?: Program;
  yaml: boolean;
  mutated: boolean;
  bindings: Map<string, Binding>;
  exports: Map<string, Binding>;
  values: Map<string, Value>;
}

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

const modules = new Map<string, ConfigModule | null>();
const origins = new WeakMap<object, Map<string, Node>>();
const suffixes = ["", ".json", ".jsonc", ".js", ".cjs", ".mjs", ".ts", ".mts", ".cts", "/index.js"];

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

function fileAt(path: string): string | undefined {
  for (const suffix of suffixes) {
    try {
      const candidate = path + suffix;
      if (statSync(candidate).isFile()) return candidate;
    } catch {}
  }

  if (path.endsWith(".js"))
    return fileAt(path.slice(0, -3) + ".ts") ?? fileAt(path.slice(0, -3) + ".mts");

  return undefined;
}

export type Loader = "import" | "require";

function entryOf(value: unknown, loader: Loader): string | undefined {
  if (typeof value === "string") return value;

  if (value && typeof value === "object" && !Array.isArray(value))
    for (const [key, target] of Object.entries(value))
      if (key === loader || key === "node" || key === "default") {
        const entry = entryOf(target, loader);
        if (entry !== undefined) return entry;
      }

  return undefined;
}

export function resolveModule(specifier: string, from: string, loader: Loader): string | undefined {
  let dir: string;
  try {
    dir = dirname(realpathSync(from));
  } catch {
    return undefined;
  }

  if (specifier.startsWith(".") || isAbsolute(specifier))
    return fileAt(resolve(dirname(from), specifier));

  const parts = specifier.split("/");
  const name = parts.splice(0, specifier.startsWith("@") ? 2 : 1).join("/");
  const subpath = parts.join("/");
  while (true) {
    const root = join(dir, "node_modules", name);
    try {
      const manifest: unknown = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
      if (!manifest || typeof manifest !== "object") return undefined;

      const exports: unknown = Reflect.get(manifest, "exports");
      const target =
        exports && typeof exports === "object"
          ? Reflect.get(exports, subpath ? `./${subpath}` : ".")
          : subpath
            ? undefined
            : exports;

      const entry = entryOf(target, loader) ?? (!subpath ? entryOf(exports, loader) : undefined);
      const main: unknown = Reflect.get(manifest, "main");
      return fileAt(
        join(root, entry ?? (subpath || (typeof main === "string" ? main : "index.js"))),
      );
    } catch {}

    const parent = dirname(dir);
    if (parent === dir) return undefined;

    dir = parent;
  }
}

function nameOf(node: Node): string | undefined {
  if (node.type === "Identifier") return node.name;
  if (node.type === "Literal" && typeof node.value === "string") return node.value;
  return undefined;
}

function memberName(node: Node): string | undefined {
  return node.type === "MemberExpression" && (!node.computed || node.property.type === "Literal")
    ? nameOf(node.property)
    : undefined;
}

function rootName(node: Node): string | undefined {
  if (node.type === "MemberExpression") return rootName(node.object);
  return node.type === "Identifier" ? node.name : undefined;
}

function exportName(node: Node): string | undefined {
  if (node.type !== "MemberExpression") return undefined;

  if (
    node.object.type === "Identifier" &&
    node.object.name === "module" &&
    memberName(node) === "exports"
  )
    return "default";

  if (node.object.type === "Identifier" && node.object.name === "exports") return memberName(node);

  return undefined;
}

type HelperKind = "flatten" | "ignore";

const HELPERS: { source: string; name: string; kind: HelperKind }[] = [
  { source: "eslint/config", name: "defineConfig", kind: "flatten" },
  { source: "eslint/config", name: "globalIgnores", kind: "ignore" },
  { source: "eslint-define-config", name: "defineConfig", kind: "flatten" },
  { source: "typescript-eslint", name: "config", kind: "flatten" },
  { source: "@eslint/compat", name: "includeIgnoreFile", kind: "ignore" },
];

function requiredSource(node: Node): string | undefined {
  return node.type === "CallExpression" &&
    node.callee.type === "Identifier" &&
    node.callee.name === "require" &&
    node.arguments[0]?.type === "Literal" &&
    typeof node.arguments[0].value === "string"
    ? node.arguments[0].value
    : undefined;
}

function helper(node: Node, module: ConfigModule): HelperKind | undefined {
  const local = node.type === "MemberExpression" ? node.object : node;
  if (local.type !== "Identifier") return undefined;

  const binding = module.bindings.get(local.name);
  const imported = binding?.imported;
  if (binding?.mutable || !imported) return undefined;

  const name = node.type === "MemberExpression" ? memberName(node) : imported.name;
  const namespace = node.type !== "MemberExpression" || ["default", "*"].includes(imported.name);
  if (!namespace) return undefined;

  return HELPERS.find((entry) => entry.source === imported.source && entry.name === name)?.kind;
}

function indexModule(module: ConfigModule): void {
  if (!module.program) return;

  const dirty = new Set<string>();
  const aliases: [string, string][] = [];
  const assignments = new Map<string, Node[]>();
  for (const statement of module.program.body) {
    const node = statement.type === "ExportNamedDeclaration" ? statement.declaration : statement;
    if (node?.type === "VariableDeclaration")
      for (const declaration of node.declarations) {
        const source =
          node.kind === "const" && declaration.init && requiredSource(declaration.init);

        if (declaration.id.type === "ObjectPattern" && source)
          for (const property of declaration.id.properties) {
            const imported =
              property.type === "Property" && !property.computed && nameOf(property.key);

            if (imported && property.value.type === "Identifier")
              module.bindings.set(property.value.name, {
                imported: { source, name: imported, loader: "require" },
              });
          }

        if (declaration.id.type !== "Identifier") continue;

        const name = declaration.id.name;
        module.bindings.set(
          name,
          source
            ? { imported: { source, name: "default", loader: "require" } }
            : { node: declaration.init ?? undefined, mutable: node.kind !== "const" },
        );

        if (statement.type === "ExportNamedDeclaration") module.exports.set(name, { local: name });

        const alias = declaration.init && rootName(declaration.init);
        if (alias) aliases.push([name, alias]);
      }

    if (statement.type === "ImportDeclaration")
      for (const specifier of statement.specifiers)
        module.bindings.set(specifier.local.name, {
          imported: {
            source: statement.source.value,
            name:
              specifier.type === "ImportDefaultSpecifier"
                ? "default"
                : specifier.type === "ImportNamespaceSpecifier"
                  ? "*"
                  : (nameOf(specifier.imported) ?? ""),
            loader: "import",
          },
        });

    if (statement.type === "ExportDefaultDeclaration")
      module.exports.set("default", { node: statement.declaration });

    if (statement.type === "ExportNamedDeclaration")
      for (const specifier of statement.specifiers)
        module.exports.set(
          nameOf(specifier.exported) ?? "",
          statement.source ? { mutable: true } : { local: nameOf(specifier.local) },
        );

    if (
      statement.type === "ExpressionStatement" &&
      statement.expression.type === "AssignmentExpression"
    ) {
      const assignment = statement.expression;
      const name = exportName(assignment.left);
      if (name !== undefined && assignment.operator === "=") {
        const previous = assignments.get(name) ?? [];
        previous.push(assignment);
        assignments.set(name, previous);
        module.exports.set(name, { node: assignment.right });
      }
    }
  }

  const permitted = new Set([...assignments.values()].filter((items) => items.length === 1).flat());

  let mutated = false;
  walk(module.program, (node) => {
    const target =
      node.type === "AssignmentExpression"
        ? node.left
        : node.type === "UpdateExpression"
          ? node.argument
          : node.type === "UnaryExpression" && node.operator === "delete"
            ? node.argument
            : undefined;

    if (target) {
      if (permitted.has(node)) return;
      if (target.type === "MemberExpression" || node.type === "UpdateExpression") mutated = true;

      const root = rootName(target);
      if (root) dirty.add(root);
    }

    if (node.type !== "CallExpression" && node.type !== "NewExpression") return;

    if (!helper(node.callee, module) && !requiredSource(node))
      for (const argument of node.arguments) {
        const passed = rootName(argument.type === "SpreadElement" ? argument.argument : argument);
        if (passed) dirty.add(passed);
      }

    if (node.type !== "CallExpression" || node.callee.type !== "MemberExpression") return;

    const callee = node.callee;
    const root = rootName(callee.object);
    if (
      root === "Reflect" ||
      (root === "Object" &&
        ["assign", "defineProperty", "defineProperties", "setPrototypeOf"].includes(
          memberName(callee) ?? "",
        ))
    )
      mutated = true;

    if (root && !helper(callee, module)) dirty.add(root);
  });

  if (mutated) {
    module.mutated = true;

    for (const binding of [...module.bindings.values(), ...module.exports.values()])
      binding.mutable = true;

    module.exports.set("default", { mutable: true });
    return;
  }

  for (let index = 0; index < aliases.length; index++)
    for (const [left, right] of aliases)
      if (dirty.has(left) || dirty.has(right)) {
        dirty.add(left);
        dirty.add(right);
      }

  for (const name of dirty) {
    const binding = module.bindings.get(name);
    if (binding) binding.mutable = true;
  }

  if (dirty.has("module") || dirty.has("exports")) module.exports.set("default", { mutable: true });
}

export function moduleAt(file: string): ConfigModule | null {
  try {
    file = realpathSync(file);
    if (modules.has(file)) return modules.get(file) ?? null;

    const text = readFileSync(file, "utf8");
    const script = /\.[cm]?[jt]s$/.test(file);
    const parsed = parse(script ? file : `${file}.js`, script ? text : `(${text}\n)`);
    const yaml = /\.ya?ml$/.test(file) || (file.endsWith(".eslintrc") && parsed.errors.length > 0);
    const module: ConfigModule = {
      file,
      text,
      program: parsed.errors.length ? undefined : parsed.program,
      yaml,
      mutated: false,
      bindings: new Map(),
      exports: new Map(),
      values: new Map(),
    };

    modules.set(file, module);

    if (script) indexModule(module);
    else if (module.program?.body[0]?.type === "ExpressionStatement")
      module.exports.set("default", { node: module.program.body[0].expression });

    return module;
  } catch {
    modules.set(file, null);
    return null;
  }
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
      if (
        node.callee.type === "Identifier" &&
        node.callee.name === "require" &&
        node.arguments[0]?.type === "Literal" &&
        typeof node.arguments[0].value === "string"
      ) {
        const name = node.arguments[0].value;
        const preset = known(name);
        const file = preset === undefined ? resolveModule(name, module.file, "require") : undefined;
        return preset !== undefined ? preset : file ? exported(file, "default", chain) : UNKNOWN;
      }

      const kind = helper(node.callee, module);
      if (kind === "ignore") return {};
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
