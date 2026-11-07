import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { dev } from "./support.ts";

test("docs/lib/catalog.json is the reference's rules, flags, usage, hooks and extensions", () => {
  const path = join(import.meta.dir, "..", "docs", "lib", "catalog.json");
  expect(readFileSync(path, "utf8")).toBe(`${dev(process.cwd(), ["catalog"]).join("\n")}\n`);
});
