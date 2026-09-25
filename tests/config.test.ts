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

test("a nested config that turns curly off wins over an ancestor that turns it on", () => {
  const root = dirWith({ ".eslintrc.json": '{ "rules": { "curly": "error" } }' });
  const nested = join(root, "packages", "legacy");
  mkdirSync(nested, { recursive: true });
  writeFileSync(join(nested, ".eslintrc.json"), '{ "rules": { "curly": "off" } }');
  expect(bracesEnforced(nested)).toBe(false);
});

test("double slashes inside a string are not a comment", () => {
  expect(
    bracesEnforced(
      dirWith({
        ".eslintrc.json": '{ "settings": { "team": "team//web" }, "rules": { "curly": "error" } }',
      }),
    ),
  ).toBe(true);

  expect(
    bracesEnforced(
      dirWith({
        "eslint.config.js": 'export default [{ name: "x // y", rules: { curly: "error" } }];',
      }),
    ),
  ).toBe(true);
});

test("biome off in the same directory does not hide eslint curly", () => {
  const dir = dirWith({
    "biome.json": '{ "linter": { "rules": { "style": { "useBlockStatements": "off" } } } }',
    "eslint.config.js": 'export default [{ rules: { curly: "error" } }];',
  });

  expect(bracesEnforced(dir)).toBe(true);
});

test("the rule name inside an ordinary string is not a setting", () => {
  const root = dirWith({ ".eslintrc.json": '{ "rules": { "curly": "error" } }' });
  const nested = join(root, "packages", "notes");
  mkdirSync(nested, { recursive: true });
  writeFileSync(
    join(nested, ".eslintrc.json"),
    '{ "settings": { "note": "curly: off", "other": "useBlockStatements: off" } }',
  );

  expect(bracesEnforced(nested)).toBe(true);
});

test("an override that turns the rule back on counts as on", () => {
  expect(
    bracesEnforced(
      dirWith({
        "biome.json":
          '{ "linter": { "rules": { "style": { "useBlockStatements": "off" } } }, "overrides": [{ "includes": ["legacy/**"], "linter": { "rules": { "style": { "useBlockStatements": { "level": "error" } } } } }] }',
      }),
    ),
  ).toBe(true);
});

test("a config ending in a line comment without a final newline still counts", () => {
  expect(
    bracesEnforced(
      dirWith({
        "biome.jsonc":
          '{ "linter": { "rules": { "style": { "useBlockStatements": "error" } } } } // end',
      }),
    ),
  ).toBe(true);
});

test("a config that cannot be parsed keeps braces", () => {
  expect(
    bracesEnforced(dirWith({ "eslint.config.js": "export default [{ rules: { curly: " })),
  ).toBe(true);
});

test("an .eslintrc written as yaml is read", () => {
  expect(bracesEnforced(dirWith({ ".eslintrc": "rules:\n  curly: error\n" }))).toBe(true);
  expect(bracesEnforced(dirWith({ ".eslintrc": "rules:\n  curly: off\n" }))).toBe(false);
});
