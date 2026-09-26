import { readFileSync, realpathSync } from "node:fs";
import type { Node, Program } from "oxc-parser";
import { walk } from "../ast.ts";
import { parse } from "../parse.ts";
import type { Value } from "./evaluate.ts";
import type { Loader } from "./find.ts";

export interface Binding {
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

const modules = new Map<string, ConfigModule | null>();

export function nameOf(node: Node): string | undefined {
  if (node.type === "Identifier") return node.name;
  if (node.type === "Literal" && typeof node.value === "string") return node.value;
  return undefined;
}

export function memberName(node: Node): string | undefined {
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

type HelperKind = "flatten" | "identity" | "ignore";

const HELPERS: { source: string; name: string; kind: HelperKind }[] = [
  { source: "eslint/config", name: "defineConfig", kind: "flatten" },
  { source: "eslint/config", name: "globalIgnores", kind: "ignore" },
  { source: "eslint-define-config", name: "defineConfig", kind: "flatten" },
  { source: "typescript-eslint", name: "config", kind: "flatten" },
  { source: "@eslint/compat", name: "includeIgnoreFile", kind: "ignore" },
  { source: "oxlint", name: "defineConfig", kind: "identity" },
];

export function requiredSource(node: Node): string | undefined {
  return node.type === "CallExpression" &&
    node.callee.type === "Identifier" &&
    node.callee.name === "require" &&
    node.arguments[0]?.type === "Literal" &&
    typeof node.arguments[0].value === "string"
    ? node.arguments[0].value
    : undefined;
}

export function helper(node: Node, module: ConfigModule): HelperKind | undefined {
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
