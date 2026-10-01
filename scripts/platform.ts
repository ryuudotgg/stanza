import { readdirSync } from "node:fs";
import { join } from "node:path";

export const compileDirectory = join(import.meta.dirname, "..", "src", "compile");

export const platforms = readdirSync(compileDirectory)
  .filter((file) => file.endsWith(".ts"))
  .map((file) => file.slice(0, -3))
  .sort();

function musl(): boolean {
  if (process.platform !== "linux") return false;
  const report = process.report?.getReport() as { header?: { glibcVersionRuntime?: string } };
  return !report?.header?.glibcVersionRuntime;
}

export function hostPlatform(): string {
  return `${process.platform}-${process.arch}${musl() ? "-musl" : ""}`;
}
