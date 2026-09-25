import { expect, test } from "bun:test";
import { mkdtempSync, writeFileSync } from "node:fs";
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
