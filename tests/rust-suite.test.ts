import { expect, test } from "bun:test";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { LAYOUTS, MODES } from "../scripts/parity.ts";
import manifest from "./rust-suite.json";
import { catalog } from "./support.ts";

const root = join(import.meta.dir, "..");

interface Replaced {
  file: string;
  test: string;
  rust?: string;
  reason?: string;
}

const replaced: Replaced[] = manifest.replaced;

const crate = join(root, "crates", "stanza", "src");

function moduleOf(file: string): string {
  return file
    .replace(/\.rs$/, "")
    .replace(/(?:\/mod|\/tests|_tests)$/, "")
    .replaceAll("/", "::");
}

const modules = new Map<string, string>();
for (const file of readdirSync(crate, { recursive: true, encoding: "utf8" })) {
  if (!file.endsWith(".rs")) continue;
  const module = moduleOf(file);
  modules.set(module, `${modules.get(module) ?? ""}\n${readFileSync(join(crate, file), "utf8")}`);
}

function hasRustTest(path: string): boolean {
  const segments = path.split("::");
  const name = segments.at(-1)!;
  const module = segments
    .slice(0, -1)
    .filter((segment) => segment !== "tests")
    .join("::");

  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const pattern = new RegExp(
    `#\\[test\\](?:\\s*#\\[(?!ignore\\b|cfg\\b)[^\\]]*\\])*\\s*fn ${escaped}\\(\\)`,
  );

  return pattern.test(modules.get(module) ?? "");
}

test("the manifest runs every test file against the binary", () => {
  const files = readdirSync(import.meta.dir)
    .filter((name) => name.endsWith(".test.ts"))
    .map((name) => `tests/${name}`)
    .sort();

  expect(manifest.entries.map((entry) => entry.file).sort()).toEqual(files);
  expect(manifest.entries.filter((entry) => "pattern" in entry)).toEqual([]);
});

test("the manifest's parity rules, modes and layouts cover the catalog and every mode", () => {
  const rules = Object.keys(catalog().rules);
  const listed: string[] = manifest.rules;
  if (listed.length > 0) expect(listed.toSorted()).toEqual(rules.toSorted());

  expect(manifest.modes.toSorted()).toEqual([...MODES].sort());
  expect(manifest.layouts.toSorted()).toEqual([...LAYOUTS].sort());
});

test("each replaced case is gone from its file and names the Rust test or reason covering it", () => {
  for (const entry of replaced) {
    const where = `${entry.file}: ${entry.test}`;
    const path = join(root, entry.file);
    if (existsSync(path))
      expect(readFileSync(path, "utf8"), `${where} is still in its file`).not.toContain(
        JSON.stringify(entry.test),
      );

    expect(Boolean(entry.rust) || Boolean(entry.reason), `${where} names no cover`).toBe(true);

    for (const name of entry.rust?.split(", ") ?? [])
      expect(hasRustTest(name), `${where} names ${name}, which is not a Rust test there`).toBe(
        true,
      );
  }
});
