import { expect, test } from "bun:test";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const files = [
  ...readdirSync(join(root, "src"), { recursive: true, encoding: "utf8" })
    .filter((file) => file.endsWith(".ts") || file === "launcher")
    .map((file) => `src/${file}`),
  ...readdirSync(join(root, "scripts"))
    .filter((file) => file.endsWith(".ts"))
    .map((file) => `scripts/${file}`),
];

const texts = new Map(files.map((file) => [file, readFileSync(join(root, file), "utf8")]));

function specifiers(text: string, pattern: RegExp): string[] {
  return [...text.matchAll(pattern)].map((match) => match[1]!);
}

const STATIC = [/\bfrom\s*["']([^"']+)["']/g, /\bimport\s*["']([^"']+)["']/g];
const DYNAMIC = /\b(?:require|import)\s*\(\s*["'`]([^"'`]+)["'`]/g;

const imports = new Map(
  [...texts].map(([file, text]) => [file, STATIC.flatMap((pattern) => specifiers(text, pattern))]),
);

const loads = new Map([...texts].map(([file, text]) => [file, specifiers(text, DYNAMIC)]));

function parser(specifier: string): boolean {
  return specifier === "oxc-parser" || specifier.startsWith("oxc-parser/");
}

function target(file: string, specifier: string): string | undefined {
  const path = resolve(root, dirname(file), specifier);
  const found = [path, `${path}.ts`, join(path, "index.ts")].find(
    (candidate) =>
      existsSync(candidate) && statSync(candidate).isFile() && texts.has(relative(root, candidate)),
  );

  return found === undefined ? undefined : relative(root, found);
}

function relatives(specifiers: Map<string, string[]>): [string, string][] {
  return [...specifiers].flatMap(([file, list]) =>
    list
      .filter((specifier) => specifier.startsWith("."))
      .map((specifier) => [file, specifier] as [string, string]),
  );
}

function edgesOf(specifiers: Map<string, string[]>): Map<string, string[]> {
  return new Map(
    [...specifiers].map(([file, list]) => [
      file,
      list.flatMap((specifier) => {
        if (!specifier.startsWith(".")) return [];
        const resolved = target(file, specifier);
        return resolved === undefined ? [] : [resolved];
      }),
    ]),
  );
}

const edges = edgesOf(imports);
const dynamicEdges = edgesOf(loads);

function closure(file: string, seen = new Set<string>()): Set<string> {
  if (seen.has(file)) return seen;

  seen.add(file);
  for (const next of edges.get(file) ?? []) closure(next, seen);

  return seen;
}

function language(file: string): string | undefined {
  return /^src\/languages\/([^/]+)\//.exec(file)?.[1];
}

function parses(file: string): boolean {
  return file.startsWith("src/languages/javascript/") || file.startsWith("src/compile/");
}

test("every relative import and load resolves to a source file", () => {
  const unresolved = [...relatives(imports), ...relatives(loads)].filter(
    ([file, specifier]) =>
      !specifier.endsWith(".json") && !specifier.endsWith(".node") && !target(file, specifier),
  );

  expect(unresolved).toEqual([]);
});

test("parser imports belong to the JavaScript language", () => {
  const outside = [...imports, ...loads].filter(
    ([file, specifiers]) => !parses(file) && specifiers.some(parser),
  );

  expect(outside.map(([file]) => file)).toEqual([]);
});

test("the engine reaches languages only through the registry and interface", () => {
  const outside = [...edges, ...dynamicEdges].flatMap(([file, targets]) =>
    file.startsWith("src/engine/")
      ? targets
          .filter(
            (target) =>
              target.startsWith("src/languages/") &&
              target !== "src/languages/index.ts" &&
              target !== "src/languages/language.ts",
          )
          .map((target) => [file, target])
      : [],
  );

  expect(outside).toEqual([]);
});

test("languages never import another language", () => {
  const crossing = [...edges, ...dynamicEdges].flatMap(([file, targets]) =>
    targets
      .filter(
        (target) =>
          language(file) !== undefined &&
          language(target) !== undefined &&
          language(file) !== language(target),
      )
      .map((target) => [file, target]),
  );

  expect(crossing).toEqual([]);
});

test("the corpus, registry and engine static closures hold no parser import", () => {
  const roots = [
    "scripts/corpus.ts",
    "src/languages/index.ts",
    ...files.filter((file) => file.startsWith("src/engine/")),
  ];

  const reached = new Set(roots.flatMap((file) => [...closure(file)]));
  const parsers = [...reached].filter((file) => imports.get(file)?.some(parser));

  expect(parsers).toEqual([]);
});
