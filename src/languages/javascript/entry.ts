import type { Entry } from "../language.ts";

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
  return (
    name.endsWith(".gen.ts") ||
    name.endsWith(".gen.tsx") ||
    name.includes(".generated.") ||
    name.endsWith(".min.js")
  );
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
  extensions: [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"],
  directories,
  skipsName,
  load: () => (require("./index.ts") as typeof import("./index.ts")).language,
};
