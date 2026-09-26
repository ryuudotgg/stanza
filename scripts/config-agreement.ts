import { existsSync, readdirSync, realpathSync, statSync, type Dirent } from "node:fs";
import { isBuiltin } from "node:module";
import { basename, dirname, extname, join, resolve, sep } from "node:path";
import type { Node } from "oxc-parser";
import { walk } from "../src/ast.ts";
import { known, moduleAt, NO_SETTING, resolveModule, type Loader } from "../src/config-static.ts";
import { braceSettings, CONFIG_FILES, type Setting } from "../src/config.ts";
import { extensions } from "../src/files.ts";

type Family = (typeof CONFIG_FILES)[number]["family"];
type Mode = "flat" | "legacy";

interface Reason {
  file: string;
  why: string;
  family: Family | null;
  cancellable: boolean;
}

interface Scan {
  reasons: Reason[];
  prettier: boolean;
}

interface Graph extends Scan {
  dependencies: { file: string; mode: Mode }[];
}

interface Source {
  specifier: string;
  extends: boolean;
  legacy: boolean;
  loader: Loader;
}

const scans = new Map<string, Scan>();
const graphs = new Map<string, Graph>();
const directories = new Map<string, Scan>();
const detections = new Map<string, Scan>();
const configNames = new Set(CONFIG_FILES.flatMap((linter) => linter.files));

function realDirectory(dir: string): string {
  if (existsSync(dir)) return realpathSync(dir);
  const parent = dirname(dir);
  return parent === dir ? dir : join(realDirectory(parent), basename(dir));
}

function reason(file: string, why: string, family: Family | null): Reason {
  return {
    file,
    why,
    family,
    cancellable:
      file.includes("/node_modules/") &&
      (family === "flat" || family === "legacy") &&
      (why.startsWith("rule ") || why.startsWith("member ") || why.startsWith("rule string ")),
  };
}

function literal(node: Node | null | undefined): string | undefined {
  return node?.type === "Literal" && typeof node.value === "string" ? node.value : undefined;
}

function textOf(node: Node): string | undefined {
  if (node.type === "TemplateLiteral" && node.expressions.length === 0)
    return node.quasis[0]?.value.cooked ?? undefined;
  return literal(node);
}

function keyOf(node: Extract<Node, { type: "Property" }>): string | undefined {
  return !node.computed && node.key.type === "Identifier" ? node.key.name : textOf(node.key);
}

function memberName(node: Extract<Node, { type: "MemberExpression" }>): string | undefined {
  return !node.computed && node.property.type === "Identifier"
    ? node.property.name
    : literal(node.property);
}

function offLiteral(node: Node | null | undefined, family: Family): boolean {
  return (
    node?.type === "Literal" &&
    (node.value === "off" || node.value === 0 || (family === "oxlint" && node.value === "allow"))
  );
}

function off(node: Node, family: Family): boolean {
  if (offLiteral(node, family)) return true;
  if (node.type === "ArrayExpression") return offLiteral(node.elements[0], family);

  return (
    family === "biome" &&
    node.type === "ObjectExpression" &&
    node.properties.some(
      (item) =>
        item.type === "Property" && keyOf(item) === "level" && literal(item.value) === "off",
    )
  );
}

function packageName(name: string): string {
  if (name.startsWith("@")) {
    const [scope, pkg] = name.split("/");
    return `${scope}/${pkg?.startsWith("eslint-config") ? pkg : pkg ? `eslint-config-${pkg}` : "eslint-config"}`;
  }

  return name.startsWith("eslint-config-") ? name : `eslint-config-${name}`;
}

function inert(specifier: string): boolean {
  const name = specifier
    .split("/")
    .slice(0, specifier.startsWith("@") ? 2 : 1)
    .join("/");

  return (
    known(specifier) === NO_SETTING ||
    [
      "typescript",
      "typescript-eslint",
      "eslint",
      "@eslint/eslintrc",
      "@eslint/compat",
      "eslint-define-config",
      "oxlint",
      "globals",
    ].includes(name) ||
    specifier.startsWith("@typescript-eslint/") ||
    specifier.startsWith("@biomejs/") ||
    specifier.startsWith("@types/") ||
    name.endsWith("parser")
  );
}

function resolveSource(
  source: Source,
  file: string,
  family: Family,
  mode: Mode,
  plugins: Set<string>,
  graph: Graph,
): void {
  const { specifier } = source;
  const dependencyMode = source.legacy ? "legacy" : mode;
  const add = (why: string) => graph.reasons.push(reason(file, why, family));
  if (
    source.extends &&
    family === "flat" &&
    dependencyMode === "flat" &&
    specifier.endsWith("/all")
  )
    add("extends */all");

  if (specifier.startsWith("node:") || isBuiltin(specifier)) return;

  if (source.extends && family === "oxlint" && !specifier.startsWith(".")) {
    add("oxlint extends unread");
    return;
  }

  if (source.extends && family === "biome" && specifier === "//") {
    let dir = dirname(file);
    while (dirname(dir) !== dir) {
      dir = dirname(dir);

      for (const name of ["biome.json", "biome.jsonc"]) {
        const candidate = join(dir, name);
        if (existsSync(candidate)) {
          graph.dependencies.push({ file: candidate, mode: dependencyMode });
          return;
        }
      }
    }

    add("unresolved //");
    return;
  }

  if (specifier === "eslint:all") {
    add("eslint:all");
    return;
  }

  if (specifier === "eslint:recommended") return;
  if (specifier === "prettier" && !source.extends) return;

  if (
    /^(prettier|eslint-config-prettier(?:\/flat)?|plugin:prettier\/recommended|eslint-plugin-prettier\/recommended)$/.test(
      specifier,
    )
  ) {
    graph.prettier = true;
    return;
  }

  if (specifier === "@eslint/js") return;

  if (
    source.extends &&
    family === "flat" &&
    dependencyMode === "flat" &&
    /^[^./@][^/]*\/.+/.test(specifier) &&
    plugins.has(specifier.split("/")[0]!)
  )
    return;

  if (inert(specifier)) return;

  const name =
    source.extends &&
    dependencyMode === "legacy" &&
    !specifier.startsWith(".") &&
    !specifier.startsWith("/") &&
    !specifier.startsWith("plugin:") &&
    !specifier.startsWith("eslint:")
      ? packageName(specifier)
      : specifier;

  const resolved = resolveModule(name, file, source.loader);
  if (!resolved) add(`unresolved ${name}`);
  else if (statSync(resolved).size > 1024 * 1024)
    graph.reasons.push(reason(realpathSync(resolved), "too large", family));
  else graph.dependencies.push({ file: resolved, mode: dependencyMode });
}

function readGraph(file: string, family: Family, mode: Mode): Graph {
  const key = `${family}:${mode}:${file}`;
  const cached = graphs.get(key);
  if (cached) return cached;

  const graph: Graph = { reasons: [], prettier: false, dependencies: [] };
  const add = (why: string) => graph.reasons.push(reason(file, why, family));
  const module = moduleAt(file);
  if (!module) add("unreadable");
  else if (module.yaml)
    for (const raw of module.text.split(/\r?\n/)) {
      const line = raw.replace(/\s#.*$/, "");
      if (
        /\bcurly\b/.test(line) &&
        !/^\s*["']?curly["']?\s*:\s*(?:(?:off|0|"off"|'off')\s*$|\[\s*(?:off|0|"off"|'off')(?=\s*[,\]]))/.test(
          line,
        )
      )
        add("yaml curly");

      if (/^\s*["']?(?:extends|overrides)["']?\s*:/.test(line))
        add("yaml extends/overrides unread");
    }
  else if (!module.program) add("unparseable");
  else {
    const rules =
      family === "biome"
        ? ["useBlockStatements"]
        : family === "oxlint"
          ? ["curly", "eslint/curly"]
          : ["curly"];

    const properties: (string | undefined)[] = [];
    const covered = new Set<Node>();
    const sources: Source[] = [];

    const plugins = new Set<string>();
    const configAliases = new Set<string>();
    const eslintrcImports = new Set<string>();
    const compatAliases = new Set<string>();

    const legacyNodes = new Set<Node>();
    let legacyDepth = 0;
    let jsAll = false;

    walk(module.program, (node) => {
      if (
        node.type === "Property" &&
        keyOf(node) === "plugins" &&
        node.value.type === "ObjectExpression"
      )
        for (const property of node.value.properties)
          if (property.type === "Property") {
            const name = keyOf(property);
            if (name !== undefined) plugins.add(name);
          }

      if (
        node.type === "ImportDeclaration" &&
        node.importKind !== "type" &&
        literal(node.source) === "@eslint/js"
      )
        for (const specifier of node.specifiers)
          if (
            specifier.type === "ImportSpecifier" &&
            specifier.importKind !== "type" &&
            (specifier.imported.type === "Identifier"
              ? specifier.imported.name
              : literal(specifier.imported)) === "configs"
          )
            configAliases.add(specifier.local.name);

      if (
        node.type === "ImportDeclaration" &&
        node.importKind !== "type" &&
        literal(node.source) === "@eslint/eslintrc"
      )
        for (const specifier of node.specifiers) eslintrcImports.add(specifier.local.name);

      if (node.type === "VariableDeclarator") {
        const required =
          node.init?.type === "CallExpression" &&
          node.init.callee.type === "Identifier" &&
          node.init.callee.name === "require" &&
          literal(node.init.arguments[0]) === "@eslint/eslintrc";

        if (required)
          if (node.id.type === "Identifier") eslintrcImports.add(node.id.name);
          else if (node.id.type === "ObjectPattern")
            for (const property of node.id.properties)
              if (property.type === "Property" && property.value.type === "Identifier")
                eslintrcImports.add(property.value.name);

        const requiredMember =
          node.init?.type === "MemberExpression" &&
          node.init.object.type === "CallExpression" &&
          node.init.object.callee.type === "Identifier" &&
          node.init.object.callee.name === "require" &&
          literal(node.init.object.arguments[0]) === "@eslint/eslintrc";

        if (requiredMember && node.id.type === "Identifier") eslintrcImports.add(node.id.name);
      }
    });

    walk(module.program, (node) => {
      if (
        node.type === "VariableDeclarator" &&
        node.id.type === "Identifier" &&
        node.init?.type === "NewExpression" &&
        node.init.callee.type === "Identifier" &&
        eslintrcImports.has(node.init.callee.name)
      )
        compatAliases.add(node.id.name);
    });

    const source = (
      node: Node | null | undefined,
      extending = false,
      legacy = mode === "legacy" || legacyDepth > 0,
      loader: Loader = "import",
    ) => {
      const specifier = literal(node);
      const addSource = (sourceSpecifier: string, sourceExtends = extending) => {
        const entry: Source = {
          specifier: sourceSpecifier,
          extends: sourceExtends,
          legacy: mode === "legacy" || legacy,
          loader: mode === "legacy" || legacy ? "require" : loader,
        };

        if (
          !sources.some(
            (candidate) =>
              candidate.specifier === entry.specifier &&
              candidate.extends === entry.extends &&
              candidate.legacy === entry.legacy &&
              candidate.loader === entry.loader,
          )
        )
          sources.push(entry);
      };

      const requireResolve =
        node?.type === "CallExpression" &&
        node.callee.type === "MemberExpression" &&
        node.callee.object.type === "Identifier" &&
        node.callee.object.name === "require" &&
        memberName(node.callee) === "resolve";

      if (extending && (mode === "legacy" || legacy))
        if (specifier !== undefined) addSource(specifier);
        else if (node?.type === "ObjectExpression") return;
        else if (requireResolve) {
          const resolvedSpecifier = literal(node.arguments[0]);
          if (resolvedSpecifier !== undefined) addSource(resolvedSpecifier, false);
          else add("extends unread");
        } else add("extends unread");
      else if (extending && family === "flat" && specifier === undefined) {
        if (node?.type === "CallExpression") add("extends unread");
      } else if (specifier !== undefined) addSource(specifier);
      else if (!extending || (node?.type !== "Identifier" && node?.type !== "ObjectExpression"))
        add(extending ? "extends unread" : "dynamic import");
    };

    walk(
      module.program,
      (node, parent) => {
        if (legacyNodes.has(node)) legacyDepth++;

        if (node.type === "Property") {
          const key = keyOf(node);
          const enclosing = properties.at(-1);

          if (rules.includes(key ?? "")) {
            covered.add(node.key);
            if (!off(node.value, family)) add(`rule ${key}`);
          }

          if (
            node.computed &&
            node.key.type !== "Literal" &&
            !(node.key.type === "TemplateLiteral" && node.key.expressions.length === 0)
          )
            add("computed key");

          if (
            family === "oxlint" &&
            key === "style" &&
            !(
              node.value.type === "Literal" &&
              (node.value.value === "off" || node.value.value === "allow")
            )
          )
            add("oxlint style");

          if (
            family === "biome" &&
            key === "style" &&
            enclosing === "rules" &&
            ((typeof literal(node.value) === "string" && literal(node.value) !== "off") ||
              (node.value.type !== "Literal" && node.value.type !== "ObjectExpression"))
          )
            add("biome style");

          if (
            family === "biome" &&
            key === "all" &&
            (enclosing === "rules" || enclosing === "style") &&
            node.value.type === "Literal" &&
            node.value.value === true
          )
            add("biome all");

          if (key === "extends")
            for (const entry of node.value.type === "ArrayExpression"
              ? node.value.elements
              : [node.value])
              source(entry, true);

          if (parent?.type === "ObjectPattern" && key === "all") jsAll = true;

          properties.push(key);
        }

        if (node.type === "MemberExpression") {
          const key = memberName(node);
          if (rules.includes(key ?? "")) {
            covered.add(node.property);

            if (
              !(
                parent?.type === "AssignmentExpression" &&
                parent.left === node &&
                off(parent.right, family)
              )
            )
              add(`member ${key}`);
          }

          const configObject =
            (node.object.type === "Identifier" &&
              (node.object.name === "configs" || configAliases.has(node.object.name))) ||
            (node.object.type === "MemberExpression" && memberName(node.object) === "configs");

          if (
            configObject &&
            ((!node.computed && key === "all") ||
              (node.computed && (literal(node.property) === undefined || key === "all")))
          )
            jsAll = true;
        }

        if (rules.includes(textOf(node) ?? "") && !covered.has(node))
          add(`rule string ${textOf(node)}`);
        if (node.type === "ImportDeclaration" && node.importKind !== "type") source(node.source);

        if (
          (node.type === "ExportNamedDeclaration" || node.type === "ExportAllDeclaration") &&
          node.exportKind !== "type" &&
          node.source
        )
          source(node.source);

        if (node.type === "ImportExpression") source(node.source);

        if (
          node.type === "TSImportEqualsDeclaration" &&
          node.moduleReference.type === "TSExternalModuleReference"
        )
          source(node.moduleReference.expression, false, undefined, "require");

        if (node.type === "CallExpression") {
          if (node.callee.type === "Identifier" && node.callee.name === "require")
            source(node.arguments[0], false, undefined, "require");

          if (
            node.callee.type === "MemberExpression" &&
            node.callee.object.type === "Identifier" &&
            node.callee.object.name === "require" &&
            memberName(node.callee) === "resolve"
          )
            source(node.arguments[0], false, undefined, "require");

          if (
            node.callee.type === "MemberExpression" &&
            node.callee.object.type === "Identifier" &&
            compatAliases.has(node.callee.object.name) &&
            memberName(node.callee) === "extends"
          )
            for (const argument of node.arguments) {
              legacyNodes.add(argument);
              source(argument, true, true, "require");
            }

          if (
            node.callee.type === "MemberExpression" &&
            node.callee.object.type === "Identifier" &&
            compatAliases.has(node.callee.object.name) &&
            memberName(node.callee) === "config"
          )
            for (const argument of node.arguments)
              if (argument.type === "ObjectExpression") {
                if (
                  argument.properties.some(
                    (property) => property.type === "Property" && keyOf(property) === "extends",
                  )
                )
                  legacyNodes.add(argument);
              } else if (
                argument.type === "CallExpression" &&
                argument.callee.type === "Identifier" &&
                argument.callee.name === "require" &&
                literal(argument.arguments[0]) !== undefined
              )
                legacyNodes.add(argument);
              else if (argument.type !== "Literal") add("extends unread");
        }
      },
      (node) => {
        if (node.type === "Property") properties.pop();
        if (legacyNodes.has(node)) legacyDepth--;
      },
    );

    if (jsAll && sources.some((entry) => entry.specifier === "@eslint/js")) add("@eslint/js all");

    for (const entry of sources) resolveSource(entry, file, family, mode, plugins, graph);
  }

  graphs.set(key, graph);
  return graph;
}

function scan(file: string, family: Family, mode: Mode): Scan {
  file = realpathSync(file);
  const key = `${family}:${mode}:${file}`;
  const cached = scans.get(key);
  if (cached) return cached;

  const result: Scan = { reasons: [], prettier: false };
  const visited = new Set<string>();
  const visit = (candidate: string, candidateMode: Mode): void => {
    candidate = realpathSync(candidate);
    const candidateKey = `${candidateMode}:${candidate}`;
    if (visited.has(candidateKey)) return;

    visited.add(candidateKey);

    const graph = readGraph(candidate, family, candidateMode);
    result.reasons.push(...graph.reasons);
    result.prettier ||= graph.prettier;

    for (const dependency of graph.dependencies) visit(dependency.file, dependency.mode);
  };

  visit(file, mode);
  scans.set(key, result);
  return result;
}

function directoryScan(dir: string): Scan {
  const cached = directories.get(dir);
  if (cached) return cached;

  const result: Scan = { reasons: [], prettier: false };
  for (const linter of CONFIG_FILES) {
    const files = linter.files.map((name) => join(dir, name)).filter((file) => existsSync(file));
    if (linter.family === "oxlint" && files.length > 1)
      result.reasons.push(
        reason(realpathSync(files[0]!), "multiple oxlint configs", linter.family),
      );

    for (const file of files) {
      const scanned = scan(file, linter.family, linter.family === "legacy" ? "legacy" : "flat");
      result.reasons.push(...scanned.reasons);
      result.prettier ||= scanned.prettier;
    }
  }

  directories.set(dir, result);
  return result;
}

function detection(dir: string): Scan {
  const cached = detections.get(dir);
  if (cached) return cached;

  const result: Scan = { reasons: [], prettier: false };

  let current = resolve(dir);
  try {
    current = realDirectory(current);

    while (true) {
      const scanned = directoryScan(current);
      result.reasons.push(...scanned.reasons);
      result.prettier ||= scanned.prettier;

      const parent = dirname(current);
      if (parent === current) break;

      current = parent;
    }
  } catch (error: unknown) {
    result.reasons.push(
      reason(current, `threw: ${error instanceof Error ? error.message : String(error)}`, null),
    );
  }

  result.reasons = [
    ...new Map(result.reasons.map((entry) => [JSON.stringify(entry), entry])).values(),
  ];

  detections.set(dir, result);
  return result;
}

export function detect(dir: string): { keep: boolean; reasons: Reason[] } {
  const { reasons } = detection(dir);
  return { keep: reasons.length > 0, reasons };
}

export function detectWithPrettier(dir: string): { keep: boolean; reasons: Reason[] } {
  const { reasons, prettier } = detection(dir);
  return {
    keep: reasons.length > 0 && !(prettier && reasons.every((entry) => entry.cancellable)),
    reasons,
  };
}

function evaluator(dir: string): Setting {
  try {
    const settings = braceSettings(dir, undefined);
    return settings.includes("on") ? "on" : settings.includes("unknown") ? "unknown" : "off";
  } catch {
    return "unknown";
  }
}

function collect(roots: string[]): { configs: string[]; sources: string[] } {
  const configs = new Set<string>();
  const sources = new Set<string>();
  const visited = new Set<string>();
  const visit = (dir: string): void => {
    dir = realpathSync(dir);
    if (
      dir.split(sep).some((part) => part === "node_modules" || part === ".git") ||
      visited.has(dir)
    )
      return;

    visited.add(dir);

    let entries: Dirent[];
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch (error: unknown) {
      if (
        error instanceof Error &&
        "code" in error &&
        (error.code === "EACCES" || error.code === "EPERM")
      ) {
        console.error(`skipped ${dir}: ${error.code}`);
        return;
      }

      throw error;
    }

    for (const entry of entries) {
      if (entry.name === "node_modules" || entry.name === ".git") continue;

      const path = join(dir, entry.name);
      if (entry.isDirectory()) visit(path);
      else if (entry.isFile() || entry.isSymbolicLink()) {
        if (configNames.has(entry.name)) configs.add(dir);
        if (extensions.has(extname(entry.name))) sources.add(dir);
      }
    }
  };

  for (const root of roots) visit(resolve(root));

  return { configs: [...configs].sort(), sources: [...sources].sort() };
}

interface Counts {
  agree: number;
  detector: number;
  on: number;
  unknown: number;
}

function count(counts: Counts, answer: Setting, keep: boolean): void {
  if ((answer !== "off") === keep) counts.agree++;
  else if (keep) counts.detector++;
  else if (answer !== "off") counts[answer]++;
}

function compare(dirs: string[], list: boolean): { pure: Counts; prettier: Counts } {
  const pure: Counts = { agree: 0, detector: 0, on: 0, unknown: 0 };
  const prettier: Counts = { ...pure };
  for (const dir of dirs) {
    const answer = evaluator(dir);
    const detected = detect(dir);
    count(pure, answer, detected.keep);
    count(prettier, answer, detectWithPrettier(dir).keep);

    if (list) {
      const disagreement = (answer !== "off") !== detected.keep;
      const details = !disagreement
        ? ""
        : detected.keep
          ? ` ${detected.reasons
              .slice(0, 3)
              .map((entry) => `${entry.file}: ${entry.why}`)
              .join("; ")}`
          : " evaluator only";

      console.log(`${answer} ${detected.keep ? "keep" : "drop"} ${dir}${details}`);
    } else if (answer !== "off" && !detected.keep)
      console.log(`only evaluator keeps ${answer} ${dir}`);
  }

  return { pure, prettier };
}

function printCounts(label: string, counts: Counts): void {
  console.log(label);
  console.log(`agree ${counts.agree}`);
  console.log(`only detector keeps ${counts.detector}`);
  console.log(
    `only evaluator keeps ${counts.on + counts.unknown} (on ${counts.on}, unknown ${counts.unknown})`,
  );
}

function run(): number {
  const roots = process.argv.slice(2);
  if (roots.length === 0) {
    console.error("Usage: bun scripts/config-agreement.ts <root>...");
    return 2;
  }

  const dirs = collect(roots);
  const configs = compare(dirs.configs, true);
  const sources = compare(dirs.sources, false);

  printCounts("config directories", configs.pure);
  printCounts("config directories with prettier", configs.prettier);
  printCounts("source directories", sources.pure);
  printCounts("source directories with prettier", sources.prettier);

  return configs.pure.on + configs.pure.unknown + sources.pure.on + sources.pure.unknown > 0
    ? 1
    : 0;
}

if (import.meta.main)
  try {
    process.exitCode = run();
  } catch (error: unknown) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 3;
  }
