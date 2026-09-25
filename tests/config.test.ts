import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { bracesEnforced } from "../src/config.ts";

function dirWith(files: Record<string, string>): string {
  const dir = mkdtempSync(join(tmpdir(), "stanza-config-"));
  for (const [name, text] of Object.entries(files)) writeFileSync(join(dir, name), text);
  return dir;
}

test("biome useBlockStatements off is not enforced", () => {
  expect(
    bracesEnforced(
      dirWith({
        "biome.json": '{ "linter": { "rules": { "style": { "useBlockStatements": "off" } } } }',
      }),
    ),
  ).toBe(false);

  expect(
    bracesEnforced(
      dirWith({ "biome.jsonc": '{ "style": { "useBlockStatements": { "level": "off" } } }' }),
    ),
  ).toBe(false);

  expect(
    bracesEnforced(
      dirWith({ "biome.json": '{ "style": { "useBlockStatements": { "level": "warn" } } }' }),
    ),
  ).toBe(true);
});

test("eslint curly is checked even when a biome config exists", () => {
  expect(
    bracesEnforced(
      dirWith({
        "biome.json": "{}",
        "eslint.config.js": 'export default [{ rules: { curly: "error" } }];',
      }),
    ),
  ).toBe(true);

  expect(
    bracesEnforced(
      dirWith({
        "eslint.config.mjs": 'export default [{ rules: { curly: ["error", "multi"] } }];',
      }),
    ),
  ).toBe(true);

  expect(bracesEnforced(dirWith({ ".eslintrc.json": '{ "rules": { "curly": 0 } }' }))).toBe(false);
  expect(bracesEnforced(dirWith({ ".eslintrc.json": '{ "rules": { "curly": ["off"] } }' }))).toBe(
    false,
  );
});

test("an ancestor config that enforces curly wins over a nested config that does not mention it", () => {
  const root = dirWith({ ".eslintrc.json": '{ "rules": { "curly": "error" } }' });
  const nested = join(root, "packages", "app");
  mkdirSync(nested, { recursive: true });
  writeFileSync(join(nested, ".eslintrc.json"), '{ "rules": { "semi": "error" } }');
  expect(bracesEnforced(nested)).toBe(true);
});

test("a commented out rule is not enforced", () => {
  expect(
    bracesEnforced(
      dirWith({ "eslint.config.js": 'export default [{ rules: { // curly: "error"\n } }];' }),
    ),
  ).toBe(false);

  expect(
    bracesEnforced(
      dirWith({ "biome.jsonc": '{ /* "useBlockStatements": "error" */ "linter": {} }' }),
    ),
  ).toBe(false);

  expect(
    bracesEnforced(
      dirWith({
        ".eslintrc.json": '{ "$schema": "https://example.com/x", "rules": { "curly": "error" } }',
      }),
    ),
  ).toBe(true);
});
