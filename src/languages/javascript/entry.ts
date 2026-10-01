import type { Entry } from "../language.ts";

const extensions = [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"];
const generatedSuffixes = [
  ...extensions.map((extension) => `.gen${extension}`),
  ...extensions
    .filter((extension) => extension.endsWith("js"))
    .map((extension) => `.min${extension}`),
];

const directories = new Set([
  "node_modules",
  "dist",
  "build",
  ".git",
  ".next",
  "out",
  "coverage",
  "migrations",
  "drizzle",
]);

function generatedName(name: string): boolean {
  return name.includes(".generated.") || generatedSuffixes.some((suffix) => name.endsWith(suffix));
}

function skipsName(name: string, keepGenerated: boolean): boolean {
  return (
    name.endsWith(".d.ts") ||
    name.endsWith(".d.mts") ||
    name.endsWith(".d.cts") ||
    (!keepGenerated && generatedName(name))
  );
}

export const entry: Entry = {
  id: "javascript",
  extensions,
  directories,
  skipsName,
  load: () => (require("./index.ts") as typeof import("./index.ts")).language,
};
