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

function rustSources(dir: string): string[] {
  return readdirSync(dir, { recursive: true, encoding: "utf8" })
    .filter((path) => path.endsWith(".rs"))
    .map((path) => readFileSync(join(dir, path), "utf8"));
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
  const sources = rustSources(join(root, "crates", "stanza", "src")).join("\n");
  for (const entry of replaced) {
    const where = `${entry.file}: ${entry.test}`;
    const path = join(root, entry.file);
    if (existsSync(path))
      expect(readFileSync(path, "utf8"), `${where} is still in its file`).not.toContain(
        JSON.stringify(entry.test),
      );

    expect(Boolean(entry.rust) || Boolean(entry.reason), `${where} names no cover`).toBe(true);

    for (const name of entry.rust?.split(", ") ?? [])
      expect(sources, `${where} names a missing Rust test`).toMatch(
        new RegExp(`#\\[test\\]\\s*fn ${name.split("::").at(-1)}\\(\\)`),
      );
  }
});
