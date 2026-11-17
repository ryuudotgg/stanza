import { expect, test } from "bun:test";
import { mkdirSync, symlinkSync, writeFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { cpuSpent } from "./budget.ts";
import {
  braceDecisions as decisionsIn,
  bracesEnforced,
  dev,
  formatterWidth,
  run,
  scratch,
  scratchGitRepository,
} from "./support.ts";

const UNKNOWN = { $unknown: true };

function braceDecisions(dir: string, extension?: string) {
  return decisionsIn(process.cwd(), [dir], extension)[0]!;
}

function exportedDefault(path: string): unknown {
  const printed = JSON.parse(dev(dirname(path), ["eval", basename(path)]).join("\n")) as Record<
    string,
    { value: unknown }
  >;

  return printed[basename(path)]!.value;
}

function dirWith(files: Record<string, string>): string {
  const dir = scratch("config");
  for (const [name, text] of Object.entries(files)) {
    mkdirSync(dirname(join(dir, name)), { recursive: true });
    writeFileSync(join(dir, name), text);
  }

  return dir;
}

test("formatter widths and their sources follow configs, overrides and defaults", () => {
  const oxfmt = dirWith({ ".oxfmtrc.json": '{ "printWidth": 120 }' });
  expect(formatterWidth(join(oxfmt, "x.ts"))).toMatchObject({
    columns: 120,
    source: { kind: "config", file: join(oxfmt, ".oxfmtrc.json") },
  });

  const biome = dirWith({ "biome.json": '{ "formatter": { "lineWidth": 90 } }' });
  expect(formatterWidth(join(biome, "x.ts"))).toMatchObject({
    columns: 90,
    source: { kind: "config", file: join(biome, "biome.json") },
  });

  const prettier = dirWith({
    ".prettierrc":
      '{ "printWidth": 70, "overrides": [{ "files": "*.test.ts", "options": { "printWidth": 60 } }] }',
  });

  expect(formatterWidth(join(prettier, "x.ts"))).toMatchObject({ columns: 70 });
  expect(formatterWidth(join(prettier, "x.test.ts"))).toMatchObject({ columns: 60 });

  const editor = dirWith({
    "package.json": '{ "devDependencies": { "prettier": "3" } }',
    ".editorconfig": "root = true\n[*]\nmax_line_length = 110\ntab_width = 4\n",
  });

  expect(formatterWidth(join(editor, "x.ts"))).toMatchObject({
    columns: 110,
    tab: 4,
    source: { kind: "config", file: join(editor, ".editorconfig") },
  });

  const defaultOxfmt = dirWith({ "package.json": '{ "devDependencies": { "oxfmt": "1" } }' });
  expect(formatterWidth(join(defaultOxfmt, "x.ts"))).toMatchObject({
    columns: 100,
    source: { kind: "default", formatter: "oxfmt" },
  });

  const fallback = dirWith({});
  expect(formatterWidth(join(fallback, "x.ts"))).toMatchObject({
    columns: 80,
    source: { kind: "fallback" },
  });
});

test("formatter selection skips lint only Biome and follows formatter dependencies upward", () => {
  const root = dirWith({
    "package.json": '{ "devDependencies": { "oxfmt": "1" } }',
    "biome.json": '{ "linter": { "enabled": true } }',
    "packages/app/package.json": '{ "name": "app" }',
    "packages/app/.oxfmtrc.json": '{ "printWidth": 120 }',
    "packages/app/.prettierrc": '{ "printWidth": 60 }',
  });

  expect(formatterWidth(join(root, "x.ts"))).toMatchObject({
    columns: 100,
    source: { kind: "default", formatter: "oxfmt" },
    unread: [],
  });

  expect(formatterWidth(join(root, "packages/app/x.ts"))).toMatchObject({
    columns: 120,
    source: { kind: "config", file: join(root, "packages/app/.oxfmtrc.json") },
  });
});

test("a symlinked path takes the width of its real path", () => {
  const root = dirWith({
    ".prettierrc":
      '{ "printWidth": 120, "overrides": [{ "files": "src/*.ts", "options": { "printWidth": 90 } }] }',
  });

  mkdirSync(join(root, "src"));
  symlinkSync(root, `${root}-link`);
  const first = formatterWidth(join(root, "src/a.ts"));
  expect(first.columns).toBe(90);
  const { columns, tab, source, unread } = first;
  expect(formatterWidth(join(`${root}-link`, "src/a.ts"))).toMatchObject({
    columns,
    tab,
    source,
    unread,
  });
});

test("formatter ties use all dependencies from the first matching package", () => {
  const root = dirWith({
    "package.json": '{ "devDependencies": { "prettier": "3", "oxfmt": "1" } }',
    "child/.prettierrc": '{ "printWidth": 70 }',
    "child/.oxfmtrc.json": '{ "printWidth": 120 }',
    "single/.oxfmtrc.json": '{ "printWidth": 120 }',
  });

  expect(formatterWidth(join(root, "x.ts"))).toMatchObject({
    columns: 80,
    source: { kind: "default", formatter: "prettier" },
  });

  expect(formatterWidth(join(root, "child/x.ts"))).toMatchObject({
    columns: 70,
    source: { kind: "config", file: join(root, "child/.prettierrc") },
  });

  expect(formatterWidth(join(root, "single/x.ts"))).toMatchObject({
    columns: 120,
    source: { kind: "config", file: join(root, "single/.oxfmtrc.json") },
  });
});

function sharedConfig(levels: number, next: (previous: string) => string): string {
  const lines = ['const c0 = { rules: { curly: "off" } };'];
  for (let level = 1; level <= levels; level++)
    lines.push(`const c${level} = ${next(`c${level - 1}`)};`);

  return `${lines.join("\n")}\nexport default c${levels};\n`;
}

test("shared flat config layers finish within one second of CPU", () => {
  const dir = dirWith({
    "eslint.config.js": sharedConfig(30, (previous) => `[${previous}, ${previous}]`),
    "x.ts": "if (ok) {\n  run();\n}\n",
  });

  const { value: decisions, ms } = cpuSpent(() => braceDecisions(dir, ".ts"));
  expect(ms).toBeLessThan(1000);
  expect(decisions).toEqual([
    { family: "flat", setting: "off", files: [join(dir, "eslint.config.js")] },
  ]);

  const checked = run({ cwd: dir }, "--check", "x.ts");
  expect(checked.stdout).toContain(": braces: ");
  expect(checked.stderr).toBe("1 finding in 1 file, fixable with --fix\n");

  expect(bracesEnforced(dir, ".ts")).toBe(false);
}, 60_000);

test("distinct flat scopes exhaust the expansion budget", () => {
  const dir = dirWith({
    "eslint.config.js": sharedConfig(
      30,
      (previous) =>
        `[{ files: ["**/*.ts"], extends: [${previous}] }, { files: ["**/*.js"], extends: [${previous}] }]`,
    ),
    "x.ts": "if (ok) {\n  run();\n}\n",
  });

  const { value: decisions, ms } = cpuSpent(() => braceDecisions(dir, ".ts"));
  expect(ms).toBeLessThan(1000);
  expect(decisions).toEqual([
    { family: "flat", setting: "unknown", files: [join(dir, "eslint.config.js")] },
  ]);

  const checked = run({ cwd: dir }, "--check", "x.ts");
  expect(checked.stdout).not.toContain(": braces: ");
  expect(checked.stderr).toBe(
    "stanza: cannot tell whether eslint.config.js enforces braces, so braces stay; pass --braces or --no-braces to settle it\nchecked 1 file, no findings\n",
  );
}, 60_000);

test("shared oxlint extends finish within one second of CPU", () => {
  const dir = dirWith({
    "oxlint.config.ts": sharedConfig(30, (previous) => `{ extends: [${previous}, ${previous}] }`),
    "x.ts": "if (ok) {\n  run();\n}\n",
  });

  const { value: decisions, ms } = cpuSpent(() => braceDecisions(dir, ".ts"));
  expect(ms).toBeLessThan(1000);
  expect(decisions).toEqual([
    { family: "oxlint", setting: "off", files: [join(dir, "oxlint.config.ts")] },
  ]);

  const checked = run({ cwd: dir }, "--check", "x.ts");
  expect(checked.stdout).toContain(": braces: ");
  expect(checked.stderr).toBe("1 finding in 1 file, fixable with --fix\n");

  expect(bracesEnforced(dir, ".ts")).toBe(false);
}, 60_000);

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

test("oxlint TypeScript defineConfig is static", () => {
  for (const setting of ["error", "off"] as const)
    expect(
      bracesEnforced(
        dirWith({
          "oxlint.config.ts": `import { defineConfig } from "oxlint";\nexport default defineConfig({ rules: { curly: "${setting}" } });`,
        }),
      ),
    ).toBe(setting === "error");
});

test("legacy eslint reads eslintConfig from package.json", () => {
  for (const [curly, enforced] of [
    ["error", true],
    ["off", false],
  ] as const) {
    const dir = dirWith({
      "package.json": JSON.stringify({ name: "x", eslintConfig: { rules: { curly } } }),
    });

    expect(bracesEnforced(dir)).toBe(enforced);
  }

  expect(braceDecisions(dirWith({ "package.json": '{ "name": "x" }' }))).toEqual([]);
});

test("flat eslint config excludes package.json eslintConfig", () => {
  const config = 'export default [{ rules: { curly: "off" } }];';
  const manifest = '{ "eslintConfig": { "rules": { "curly": "error" } } }';
  const sameDir = dirWith({ "eslint.config.js": config, "package.json": manifest });
  const parent = dirWith({ "eslint.config.js": config, "child/package.json": manifest });

  expect(bracesEnforced(sameDir)).toBe(false);
  expect(bracesEnforced(join(parent, "child"))).toBe(false);
});

test("unresolved and invalid package.json eslintConfig keep braces", () => {
  expect(
    bracesEnforced(dirWith({ "package.json": '{ "eslintConfig": { "extends": ["missing"] } }' })),
  ).toBe(true);

  for (const manifest of ['{ "eslintConfig": {}, }', '{ "eslintConfig": "off" }']) {
    const dir = dirWith({ "package.json": manifest });

    expect(bracesEnforced(dir)).toBe(true);
    expect(braceDecisions(dir)).toEqual([
      { family: "legacy", setting: "unknown", files: [join(dir, "package.json")] },
    ]);
  }
});

test("package.json eslintConfig resolves extends and applies overrides", () => {
  const dir = dirWith({
    "package.json": JSON.stringify({
      eslintConfig: {
        extends: ["strict"],
        overrides: [{ files: ["src/**"], rules: { curly: "off" } }],
      },
    }),
    "node_modules/eslint-config-strict/package.json": '{ "main": "index.json" }',
    "node_modules/eslint-config-strict/index.json": '{ "rules": { "curly": "error" } }',
  });

  expect(bracesEnforced(dir)).toBe(true);
  expect(bracesEnforced(join(dir, "src"), ".ts")).toBe(false);
});

test("package.json eslintConfig root stops the legacy upward walk", () => {
  const root = dirWith({
    ".eslintrc.json": '{ "rules": { "curly": "error" } }',
    "child/package.json": '{ "eslintConfig": { "root": true } }',
  });

  expect(bracesEnforced(join(root, "child"))).toBe(false);
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

test("biome style group severity enforces braces", () => {
  expect(
    bracesEnforced(dirWith({ "biome.json": '{ "linter": { "rules": { "style": "error" } } }' })),
  ).toBe(true);
});

test("plugin packages contribute no setting through imports, requires, spreads or extends", () => {
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

test("basePath is always uncertain and language does not scope a layer", () => {
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

test("member mutations invalidate all module bindings and exports", () => {
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
    expect(exportedDefault(join(root, "eslint.config.js"))).toEqual(UNKNOWN);
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

test("namespace collisions, cycles, oversized arrays and thrown reads are safe", () => {
  for (const expression of ["{ ...a, ...b }", '{ ...a, rules: { curly: "off" } }']) {
    const root = dirWith({
      "a.js": 'export const rules = { curly: "error" };',
      "b.js": 'export const rules = { curly: "off" };',
      "eslint.config.js": `import * as a from "./a.js"; import * as b from "./b.js"; export default [${expression}];`,
    });

    expect(bracesEnforced(root)).toBe(false);
  }

  const self = dirWith({ "eslint.config.js": "const a = { rules: a.rules }; export default a;" });
  expect(exportedDefault(join(self, "eslint.config.js"))).toEqual({ rules: UNKNOWN });
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
    expect(exportedDefault(join(root, "eslint.config.js"))).toEqual(UNKNOWN);
    expect(bracesEnforced(root)).toBe(true);
  }
});

test("YAML rule mentions outside the matched line remain unknown", () => {
  for (const yaml of [
    "rules: { curly: error }\n",
    "rules:\n  curly: off\noverrides: [{ rules: { curly: error } }]\n",
    "rules:\n  curly: off\nother:\n  curly: error\n",
  ])
    expect(bracesEnforced(dirWith({ ".eslintrc.yml": yaml }))).toBe(true);
});

test("Biome style option strings are not rule group severities", () => {
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

test("60 scoped config objects fold across 700 directories within one second of CPU", () => {
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

  const { value: results, ms } = cpuSpent(() =>
    decisionsIn(root, directories).map((decisions) =>
      decisions.some((decision) => decision.setting !== "off"),
    ),
  );

  expect(results.every(Boolean)).toBe(true);
  expect(ms).toBeLessThan(1000);
}, 60_000);

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

test("package exports conditions are matched in the order the package lists them", () => {
  const root = dirWith({
    "eslint.config.js": 'import acme from "@acme/eslint-config"; export default [acme];',
    "node_modules/@acme/eslint-config/package.json": JSON.stringify({
      name: "@acme/eslint-config",
      exports: { ".": { default: "./on.js", import: "./off.js" } },
    }),
    "node_modules/@acme/eslint-config/on.js": 'export default { rules: { curly: "error" } };',
    "node_modules/@acme/eslint-config/off.js": 'export default { rules: { curly: "off" } };',
  });

  expect(bracesEnforced(root)).toBe(true);
});

test("config notices read the same from a run and from the hook, each under its own name", () => {
  const notices = [
    "cannot tell whether eslint.config.js enforces braces, so braces stay; pass --braces or --no-braces to settle it",
    "cannot read the line width from .prettierrc.json, so the next source sets it",
  ];

  const files = {
    "eslint.config.js": "export default makeConfig();\n",
    ".prettierrc.json": "{",
    "a.ts": "export const a = 1;\n",
  };

  const cwd = scratchGitRepository({ files });
  const checked = run({ cwd }, "--check", "a.ts");
  expect(checked).toEqual({
    code: 0,
    stdout: "",
    stderr: `${notices.map((line) => `stanza: ${line}\n`).join("")}checked 1 file, no findings\n`,
  });

  const input = {
    cwd,
    hook_event_name: "PreToolUse",
    tool_name: "Write",
    tool_input: { file_path: join(cwd, "a.ts"), content: files["a.ts"] },
  };

  const env = { ...process.env, AGENT_HOOKS: "1" };
  const hooked = run({ cwd, env, stdin: Buffer.from(JSON.stringify(input)) }, "hook");
  expect(hooked).toEqual({
    code: 0,
    stdout: "",
    stderr: notices.map((line) => `stanza hook: ${line}\n`).join(""),
  });
});
