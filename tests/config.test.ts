import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { globReach } from "../src/config-glob.ts";
import { exported, property, UNKNOWN } from "../src/config-static.ts";
import { bracesEnforced } from "../src/config.ts";

function dirWith(files: Record<string, string>): string {
  const dir = mkdtempSync(join(tmpdir(), "stanza-config-"));
  for (const [name, text] of Object.entries(files)) {
    mkdirSync(dirname(join(dir, name)), { recursive: true });
    writeFileSync(join(dir, name), text);
  }

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
  expect(bracesEnforced(root)).toBe(true);
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

test("an override that turns the rule back on applies only to its directory", () => {
  const root = dirWith({
    "biome.json":
      '{ "linter": { "rules": { "style": { "useBlockStatements": "off" } } }, "overrides": [{ "includes": ["legacy/**"], "linter": { "rules": { "style": { "useBlockStatements": { "level": "error" } } } } }] }',
  });

  expect(bracesEnforced(root)).toBe(false);
  expect(bracesEnforced(join(root, "legacy"))).toBe(true);
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

test("yaml list forms of curly are read", () => {
  expect(bracesEnforced(dirWith({ ".eslintrc": "rules:\n  curly: [off]\n" }))).toBe(false);
  expect(bracesEnforced(dirWith({ ".eslintrc": 'rules:\n  curly: ["error", "multi"]\n' }))).toBe(
    true,
  );

  expect(bracesEnforced(dirWith({ ".eslintrc.yml": "rules:\n  curly:\n    - off\n" }))).toBe(false);
  expect(
    bracesEnforced(dirWith({ ".eslintrc.yaml": "rules:\n  curly:\n    - error\n    - multi\n" })),
  ).toBe(true);

  expect(
    bracesEnforced(dirWith({ ".eslintrc.yml": "rules:\n  curly: off # keep braces optional\n" })),
  ).toBe(false);
});

for (const [entry, text] of [
  ["index.json", '{ "rules": { "curly": "error" } }'],
  ["index.js", 'module.exports = { rules: { curly: "error" } };'],
])
  test(`legacy extends reads a shared package with a ${entry} entry`, () => {
    const root = dirWith({
      ".eslintrc.json": '{ "extends": ["@acme/eslint-config"] }',
      "node_modules/@acme/eslint-config/package.json": JSON.stringify({
        name: "@acme/eslint-config",
        main: entry,
      }),
      [`node_modules/@acme/eslint-config/${entry}`]: text!,
    });

    expect(bracesEnforced(root)).toBe(true);
  });

test("flat imports read a shared package object", () => {
  const root = dirWith({
    "eslint.config.js": 'import acme from "@acme/eslint-config"; export default [acme];',
    "node_modules/@acme/eslint-config/package.json":
      '{"name":"@acme/eslint-config","main":"index.js"}',
    "node_modules/@acme/eslint-config/index.js": 'export default { rules: { curly: "error" } };',
  });

  expect(bracesEnforced(root)).toBe(true);
});

test("a shared package can turn curly off", () => {
  const root = dirWith({
    ".eslintrc.json": '{ "extends": ["@acme/eslint-config"] }',
    "node_modules/@acme/eslint-config/package.json":
      '{"name":"@acme/eslint-config","main":"index.json"}',
    "node_modules/@acme/eslint-config/index.json": '{ "rules": { "curly": "off" } }',
  });

  expect(bracesEnforced(root)).toBe(false);
});

test("biome JSONC overrides cover the named directory and its descendants", () => {
  const root = dirWith({
    "biome.jsonc":
      '{ // legacy code\n "overrides": [{ "includes": ["src/legacy/**"], "linter": { "rules": { "style": { "useBlockStatements": "error" } } } }] }',
  });

  expect(bracesEnforced(join(root, "src", "legacy"))).toBe(true);
  expect(bracesEnforced(join(root, "src", "legacy", "deep"))).toBe(true);
  expect(bracesEnforced(join(root, "src"))).toBe(false);
  expect(bracesEnforced(root)).toBe(false);
});

test("an unresolvable extends keeps braces", () => {
  expect(bracesEnforced(dirWith({ ".eslintrc.json": '{ "extends": ["missing"] }' }))).toBe(true);
});

test("the known prettier preset turns curly off without an installed package", () => {
  const root = dirWith({
    ".eslintrc.json": '{ "extends": ["prettier"] }',
    "nested/.eslintrc.json": '{ "extends": ["missing"] }',
  });

  expect(bracesEnforced(root)).toBe(false);
  expect(bracesEnforced(join(root, "nested"))).toBe(true);
});

test("an unevaluable spread in a flat config keeps braces", () => {
  expect(
    bracesEnforced(
      dirWith({
        "eslint.config.js": 'export default [{ ...makeConfig(), rules: { curly: "off" } }];',
      }),
    ),
  ).toBe(true);
});

test("biome style group severity enforces braces", () => {
  expect(
    bracesEnforced(dirWith({ "biome.json": '{ "linter": { "rules": { "style": "error" } } }' })),
  ).toBe(true);
});

test("a stale nested legacy config cannot disable the flat config", () => {
  const root = dirWith({
    "eslint.config.js": 'export default [{ rules: { curly: "error" } }];',
    "nested/.eslintrc.json": '{ "rules": { "curly": "off" } }',
  });

  expect(bracesEnforced(join(root, "nested"))).toBe(true);
});

test("1: plugin packages contribute no setting through imports, requires, spreads or extends", () => {
  for (const source of [
    "eslint-plugin-demo",
    "@demo/eslint-plugin",
    "@demo/eslint-plugin-extra",
    "@typescript-eslint/eslint-plugin",
  ])
    for (const binding of [
      `import plugin from "${source}";`,
      `const plugin = require("${source}");`,
    ]) {
      const root = dirWith({
        "eslint.config.js": `${binding}
          export default [plugin.configs.recommended, ...plugin.configs.recommended, plugin.configs[unknownName],
            { ...plugin.configs.recommended, rules: { ...plugin.configs.recommended.rules } },
            { plugins: { demo: plugin }, extends: ["demo/recommended"] }];`,
      });

      expect(bracesEnforced(root)).toBe(false);
    }

  for (const preset of [
    "plugin:demo/recommended",
    "plugin:@demo/strict",
    "plugin:@demo/extra/strict",
  ])
    expect(bracesEnforced(dirWith({ ".eslintrc.json": JSON.stringify({ extends: preset }) }))).toBe(
      false,
    );

  for (const preset of ["plugin:prettier/recommended", "eslint-plugin-prettier/recommended"])
    expect(
      bracesEnforced(
        dirWith({ ".eslintrc.json": JSON.stringify({ extends: ["eslint:all", preset] }) }),
      ),
    ).toBe(false);
});

test("2: symlinked configs retain their discovered anchors", () => {
  const root = dirWith({
    "shared/config.js": 'export default [{ files: ["src/**"], rules: { curly: "error" } }];',
    "first/src/a.ts": "",
    "second/src/a.ts": "",
    "second/lib/a.ts": "",
  });

  for (const name of ["first", "second"])
    symlinkSync(join(root, "shared/config.js"), join(root, name, "eslint.config.js"));

  expect(bracesEnforced(join(root, "first/src"))).toBe(true);
  expect(bracesEnforced(join(root, "second/src"))).toBe(true);
  expect(bracesEnforced(join(root, "second/lib"))).toBe(false);

  const biome = dirWith({
    "shared/config.json":
      '{ "overrides": [{ "includes": ["pkg/src/**"], "style": { "useBlockStatements": "error" } }] }',
    "app/pkg/biome.json":
      '{ "root": false, "extends": "//", "overrides": [{ "includes": ["other/**"], "style": { "useBlockStatements": "off" } }] }',
  });

  symlinkSync(join(biome, "shared/config.json"), join(biome, "app/biome.json"));
  expect(bracesEnforced(join(biome, "app/pkg/src"))).toBe(true);
});

test("3: unsupported glob syntax remains uncertain in includes and exclusions", () => {
  for (const token of ["?", "[", "]", "(", ")", "+", "@", "!"])
    for (const pattern of [`unrelated/${token}.ts`, `!unrelated/${token}.ts`]) {
      expect(globReach([pattern], "src", false)).toBe("some");
      expect(globReach([pattern], "src", true)).toBe("some");

      const included = dirWith({
        "eslint.config.js": `export default [{ files: [${JSON.stringify(pattern)}], rules: { curly: "error" } }];`,
      });

      const excluded = dirWith({
        "eslint.config.js": `export default [{ rules: { curly: "error" } }, { ignores: [${JSON.stringify(pattern)}], rules: { curly: "off" } }];`,
      });

      expect(bracesEnforced(join(included, "src"))).toBe(true);
      expect(bracesEnforced(join(excluded, "src"))).toBe(true);
    }
});

test("4: basePath is always uncertain and language does not scope a layer", () => {
  const root = dirWith({
    "eslint.config.js":
      'export default [{ basePath: "src", files: ["other/**"], rules: { curly: "error" } }];',
  });

  expect(bracesEnforced(root)).toBe(true);
  expect(bracesEnforced(join(root, "src/other"))).toBe(true);

  const language = dirWith({
    "eslint.config.js":
      'export default [{ rules: { curly: "error" } }, { language: "js/js", rules: { curly: "off" } }];',
  });

  expect(bracesEnforced(language)).toBe(false);
});

test("5: biome double slash inherits the nearest ancestor at its own anchor", () => {
  const root = dirWith({
    "biome.json": '{ "style": { "useBlockStatements": "error" } }',
    "workspace/biome.jsonc":
      '{ "root": false, "style": { "useBlockStatements": "off" }, "overrides": [{ "includes": ["pkg/src/**"], "style": { "useBlockStatements": "error" } }] }',
    "workspace/pkg/biome.json":
      '{ "root": false, "extends": "//", "overrides": [{ "includes": ["other/**"], "style": { "useBlockStatements": "off" } }] }',
  });

  expect(bracesEnforced(join(root, "workspace/pkg/src"))).toBe(true);
  expect(bracesEnforced(join(root, "workspace/pkg/lib"))).toBe(false);

  const overridden = dirWith({
    "biome.json": '{ "style": { "useBlockStatements": "error" } }',
    "pkg/biome.json":
      '{ "root": false, "extends": "//", "style": { "useBlockStatements": "off" } }',
  });

  expect(bracesEnforced(join(overridden, "pkg"))).toBe(false);
});

test("6: member mutations invalidate all module bindings and exports", () => {
  for (const mutation of [
    'const { rules } = config; rules.curly = "error";',
    'for (const item of [config]) item.rules.curly = "error";',
    'function change(item) { item.rules.curly = "error"; }',
    'const list = [config]; list[0].rules.curly = "error";',
    "config.rules.curly++;",
    "delete config.rules.curly;",
    "Object.assign(config.rules, strict);",
    'Object.defineProperty(config.rules, "curly", { value: "error" });',
    'Object.defineProperties(config.rules, { curly: { value: "error" } });',
    "Object.setPrototypeOf(config.rules, strict);",
    'Reflect.set(config.rules, "curly", "error");',
    'Reflect.get(config.rules, "curly");',
  ]) {
    const root = dirWith({
      "strict.js": 'export default { curly: "error" };',
      "eslint.config.js": `import strict from "./strict.js";
        const config = { rules: { curly: "off" } };
        const clean = { rules: { curly: "off" } };
        ${mutation}
        export { config, clean }; export default clean;`,
    });

    expect(bracesEnforced(root)).toBe(true);
    expect(exported(join(root, "eslint.config.js"), "config")).toBe(UNKNOWN);
    expect(exported(join(root, "eslint.config.js"), "clean")).toBe(UNKNOWN);
    expect(exported(join(root, "eslint.config.js"), "*")).toBe(UNKNOWN);
  }

  for (const assignment of [
    "try { module.exports = {}; } catch {}",
    "if (true) module.exports = {};",
    "{ module.exports = {}; }",
    "function change() { module.exports = {}; }",
    "module.exports = {};",
    "try { exports.extra = {}; } catch {}",
    "exports.extra = {}; exports.extra = {};",
  ])
    expect(
      bracesEnforced(
        dirWith({ ".eslintrc.cjs": `module.exports = { rules: { curly: "off" } }; ${assignment}` }),
      ),
    ).toBe(true);

  expect(
    bracesEnforced(
      dirWith({
        ".eslintrc.cjs": 'module.exports = { rules: { curly: "off" } }; exports.extra = {};',
      }),
    ),
  ).toBe(false);

  const alias = dirWith({
    "eslint.config.js":
      "const config = []; const alias = config; alias.push({}); export default config;",
  });

  expect(bracesEnforced(alias)).toBe(true);
});

test("7: only imported table helpers can flatten or ignore arguments", () => {
  for (const source of ["eslint/config", "eslint-define-config"])
    expect(
      bracesEnforced(
        dirWith({
          "eslint.config.js": `import { defineConfig as define } from "${source}"; export default define({ rules: { curly: "off" } });`,
        }),
      ),
    ).toBe(false);

  for (const script of [
    'import { defineConfig } from "elsewhere"; export default defineConfig({});',
    "function defineConfig() { return []; } export default defineConfig({});",
    "export default defineConfig({});",
    'import { globalIgnores } from "elsewhere"; export default [globalIgnores([])];',
    "export default [globalIgnores([])];",
    'import tool from "elsewhere"; export default tool.config({});',
    "const tool = { config() { return []; } }; export default tool.config({});",
    'import { defineConfig } from "eslint/config"; export default defineConfig(makeConfig());',
  ])
    expect(bracesEnforced(dirWith({ "eslint.config.js": script }))).toBe(true);

  expect(
    bracesEnforced(
      dirWith({
        "eslint.config.js":
          'import { defineConfig, globalIgnores } from "eslint/config"; export default defineConfig(globalIgnores(["build/**"]), { rules: { curly: "off" } });',
      }),
    ),
  ).toBe(false);

  expect(
    bracesEnforced(
      dirWith({
        "eslint.config.js":
          'import tseslint from "typescript-eslint"; export default tseslint.config({ rules: { curly: "off" } });',
      }),
    ),
  ).toBe(false);
});

test("8: namespace collisions, cycles, oversized arrays and thrown reads are safe", () => {
  for (const expression of ["{ ...a, ...b }", '{ ...a, rules: { curly: "off" } }']) {
    const root = dirWith({
      "a.js": 'export const rules = { curly: "error" };',
      "b.js": 'export const rules = { curly: "off" };',
      "eslint.config.js": `import * as a from "./a.js"; import * as b from "./b.js"; export default [${expression}];`,
    });

    expect(bracesEnforced(root)).toBe(false);
  }

  const broken = dirWith({ "eslint.config.js": "export default {};" });
  const value = exported(join(broken, "eslint.config.js"), "default");
  let reads = 0;
  Object.defineProperty(value, "rules", {
    get() {
      reads++;
      throw new Error("unreadable rules");
    },
  });

  expect(bracesEnforced(broken)).toBe(true);
  expect(bracesEnforced(broken)).toBe(true);
  expect(reads).toBe(1);

  const self = dirWith({ "eslint.config.js": "const a = { rules: a.rules }; export default a;" });
  expect(property(exported(join(self, "eslint.config.js"), "default"), "rules")).toBe(UNKNOWN);
  expect(bracesEnforced(self)).toBe(true);

  for (const script of [
    "const a = { extends: [b] }; const b = { extends: [a] }; export default a;",
    "const a = b; const b = a; export default a;",
    "const level = { level }; export default [{ rules: { curly: level } }];",
  ]) {
    const root = dirWith({ "eslint.config.js": script });
    expect(bracesEnforced(root)).toBe(true);
    expect(bracesEnforced(root)).toBe(true);
  }

  for (const script of [
    `export default [${Array.from({ length: 10001 }, () => "{}").join(",")}];`,
    `const base = [${Array.from({ length: 10000 }, () => "{}").join(",")}]; export default [...base, {}];`,
    `import { defineConfig } from "eslint/config"; const base = [${Array.from({ length: 10000 }, () => "{}").join(",")}]; export default defineConfig(base, {});`,
    `const a0 = [{}]; ${Array.from({ length: 24 }, (_, index) => `const a${index + 1} = [...a${index}, ...a${index}];`).join("\n")} export default a24;`,
  ]) {
    const root = dirWith({ "eslint.config.js": script });
    expect(exported(join(root, "eslint.config.js"), "default")).toBe(UNKNOWN);
    expect(bracesEnforced(root)).toBe(true);
  }
});

test("9: YAML rule mentions outside the matched line remain unknown", () => {
  for (const yaml of [
    "rules: { curly: error }\n",
    "rules:\n  curly: off\noverrides: [{ rules: { curly: error } }]\n",
    "rules:\n  curly: off\nother:\n  curly: error\n",
  ])
    expect(bracesEnforced(dirWith({ ".eslintrc.yml": yaml }))).toBe(true);
});

test("10: typescript presets are arrays and recommended JS rules are spreadable", () => {
  for (const script of [
    'import ts from "typescript-eslint"; export default [...ts.configs.recommended];',
    'import ts from "typescript-eslint"; export default ts.config({ extends: [...ts.configs.strict] });',
    'import ts from "typescript-eslint"; export default [{ extends: ts.configs.anything }];',
    'import js from "@eslint/js"; export default [{ rules: { ...js.configs.recommended.rules } }];',
    'import js from "@eslint/js"; import ts from "typescript-eslint"; import prettier from "eslint-config-prettier"; export default ts.config(js.configs.recommended, ...ts.configs.recommended, prettier);',
  ])
    expect(bracesEnforced(dirWith({ "eslint.config.js": script }))).toBe(false);
});

test("11: Biome style option strings are not rule group severities", () => {
  for (const config of [
    { linter: { rules: { style: { useImportType: { options: { style: "separatedType" } } } } } },
    { javascript: { formatter: { style: "interface" } } },
    { style: "interface" },
  ])
    expect(bracesEnforced(dirWith({ "biome.json": JSON.stringify(config) }))).toBe(false);

  expect(
    bracesEnforced(dirWith({ "biome.json": '{ "linter": { "rules": { "style": "warn" } } }' })),
  ).toBe(true);
});

test("12: 60 scoped config objects fold across 700 directories within one second", () => {
  const configs = Array.from({ length: 60 }, (_, index) => ({
    files: [
      `packages/p${index}/**/*.{ts,tsx}`,
      `apps/a${index}/src/**/*.{js,jsx,mjs}`,
      "**/*.test.{ts,tsx}",
    ],
    rules: { curly: "error" },
  }));

  const root = dirWith({ "eslint.config.js": `export default ${JSON.stringify(configs)};` });
  const directories = Array.from({ length: 700 }, (_, index) =>
    join(root, `packages/p${index % 60}/src/d${index}/e`),
  );

  for (const dir of directories) mkdirSync(dir, { recursive: true });

  const start = performance.now();
  const results = directories.map((dir) => bracesEnforced(dir));
  const elapsed = performance.now() - start;
  expect(results.every(Boolean)).toBe(true);
  expect(elapsed).toBeLessThan(1000);
});

test("13: family-specific rule reads work without redundant rule parameters", () => {
  for (const [curly, blocks, expected] of [
    ["off", "error", true],
    ["error", "off", true],
    ["off", "off", false],
  ] as const) {
    const root = dirWith({
      ".eslintrc.yml": `rules:\n  curly: ${curly}\n`,
      "biome.json": JSON.stringify({
        linter: { rules: { style: { useBlockStatements: blocks } } },
      }),
    });

    expect(bracesEnforced(root)).toBe(expected);
  }
});

test("a destructured require of defineConfig flattens the config", () => {
  const root = dirWith({
    "eslint.config.js":
      'const { defineConfig } = require("eslint/config");\nmodule.exports = defineConfig([{ rules: { curly: "off" } }, { ignores: ["dist/*"] }]);',
  });

  expect(bracesEnforced(root)).toBe(false);
});

test("includeIgnoreFile from @eslint/compat contributes no rules", () => {
  const root = dirWith({
    "eslint.config.js":
      'import { includeIgnoreFile } from "@eslint/compat";\nimport tseslint from "typescript-eslint";\nexport default tseslint.config(includeIgnoreFile("/x/.gitignore"), { rules: { curly: "off" } });',
  });

  expect(bracesEnforced(root)).toBe(false);
});

test("a glob with too many brace alternatives is uncertain instead of expanded", () => {
  const groups = "{a,b}".repeat(30);
  const start = performance.now();

  expect(globReach([`src/${groups}/*.ts`], "src", false)).toBe("some");
  expect(globReach([`${groups}/**`], "x", false)).toBe("some");
  expect(performance.now() - start).toBeLessThan(200);
});

test("an ESM import resolves the package import entry", () => {
  const root = dirWith({
    "eslint.config.js": 'import acme from "@acme/eslint-config"; export default [acme];',
    "node_modules/@acme/eslint-config/package.json": JSON.stringify({
      name: "@acme/eslint-config",
      exports: { ".": { require: "./off.cjs", import: "./on.js" } },
    }),
    "node_modules/@acme/eslint-config/off.cjs": 'module.exports = { rules: { curly: "off" } };',
    "node_modules/@acme/eslint-config/on.js": 'export default { rules: { curly: "error" } };',
  });

  expect(bracesEnforced(root)).toBe(true);
});

test("a config passed to an unknown function is uncertain", () => {
  const root = dirWith({
    "eslint.config.js":
      'import { strict } from "./strict.js";\nconst config = { rules: { curly: "off" } };\nstrict(config);\nexport default [config];',
    "strict.js": 'export function strict(config) { config.rules.curly = "error"; }',
  });

  expect(bracesEnforced(root)).toBe(true);
});

test("flat files globs are resolved per file extension", () => {
  const root = dirWith({
    "eslint.config.js":
      'export default [{ rules: { curly: "error" } }, { files: ["src/**/*.ts"], rules: { curly: "off" } }];',
  });

  const src = join(root, "src");
  expect(bracesEnforced(src, ".ts")).toBe(false);
  expect(bracesEnforced(src, ".js")).toBe(true);
  expect(bracesEnforced(src)).toBe(true);
});

test("a named biome rule wins over its group severity", () => {
  expect(
    bracesEnforced(
      dirWith({
        "biome.json":
          '{ "linter": { "rules": { "all": true, "style": { "useBlockStatements": "off" } } } }',
      }),
    ),
  ).toBe(false);
});
