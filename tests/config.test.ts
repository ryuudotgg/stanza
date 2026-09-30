import { expect, test } from "bun:test";
import { mkdirSync, realpathSync, symlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { globReach } from "../src/languages/javascript/config/glob.ts";
import { exported, property, UNKNOWN } from "../src/languages/javascript/config/evaluate.ts";
import { braceDecisions, bracesEnforced } from "../src/languages/javascript/config/index.ts";
import { formatterWidth } from "../src/languages/javascript/config/width.ts";
import { formatText, stepSettings } from "../src/step.ts";
import { judge } from "../scripts/corpus.ts";
import { run, scratch } from "./support.ts";

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

test("editorconfig sections and files resolve each property in order", () => {
  const root = dirWith({
    "package.json": '{ "devDependencies": { "prettier": "3" } }',
    ".editorconfig":
      "[*]\nmax_line_length = 105\nindent_size = 3\ntab_width = 8\n[*.ts]\nmax_line_length = 110\nindent_size = tab\n[*.ts]\nmax_line_length = off\nmax_line_length = 115\n",
    "child/.editorconfig":
      "root = true\n[*]\nmax_line_length = nonsense\ntab_width = 6\n[*.ts]\nmax_line_length = 90\nindent_size = tab\n",
  });

  expect(formatterWidth(join(root, "x.ts"))).toMatchObject({
    columns: 115,
    tab: 8,
    source: { kind: "config", file: join(root, ".editorconfig") },
  });

  expect(formatterWidth(join(root, "child/x.ts"))).toMatchObject({
    columns: 90,
    tab: 6,
    source: { kind: "config", file: join(root, "child/.editorconfig") },
  });
});

test("a nearer editorconfig tab width beats a parent indent size", () => {
  const root = dirWith({
    "package.json": '{ "devDependencies": { "prettier": "3" } }',
    ".editorconfig": "[*]\nmax_line_length = 110\nindent_size = 3\n",
    "child/.editorconfig": "[*]\nmax_line_length = off\nmax_line_length = invalid\ntab_width = 6\n",
  });

  expect(formatterWidth(join(root, "child/x.ts"))).toMatchObject({
    columns: 80,
    tab: 6,
    source: { kind: "default", formatter: "prettier" },
  });
});

test("Biome overrides merge formatter fields into the base", () => {
  const root = dirWith({
    "biome.json": JSON.stringify({
      formatter: { lineWidth: 120, indentWidth: 4 },
      javascript: { formatter: { lineWidth: 110, indentWidth: 6 } },
      overrides: [
        {
          includes: ["src/**"],
          formatter: { indentWidth: 2 },
          javascript: { formatter: { indentWidth: 3 } },
        },
      ],
    }),
  });

  expect(formatterWidth(join(root, "src/a.ts"))).toMatchObject({ columns: 110, tab: 3 });
  expect(formatterWidth(join(root, "other.ts"))).toMatchObject({ columns: 110, tab: 6 });
});

test("Biome file filters skip a config that excludes the target", () => {
  for (const files of [
    { includes: ["src/**"] },
    { include: ["src/**"] },
    { ignore: ["other.ts"] },
  ]) {
    const root = dirWith({
      ".prettierrc": '{ "printWidth": 120 }',
      "child/biome.json": JSON.stringify({ files, formatter: { lineWidth: 70 } }),
    });

    expect(formatterWidth(join(root, "child/other.ts"))).toMatchObject({ columns: 120 });
  }

  const root = dirWith({
    ".prettierrc": '{ "printWidth": 120 }',
    "child/biome.json": '{ "formatter": { "lineWidth": 70, "includes": ["src/**"] } }',
  });

  expect(formatterWidth(join(root, "child/other.ts"))).toMatchObject({ columns: 120 });
});

test("unread editorconfig files fall back without a finding", () => {
  const root = dirWith({ "package.json": '{ "devDependencies": { "prettier": "3" } }' });
  mkdirSync(join(root, ".editorconfig"));

  expect(formatterWidth(join(root, "a.ts"))).toMatchObject({
    columns: 80,
    unread: [join(root, ".editorconfig")],
  });

  expect(
    formatText(join(root, "a.ts"), "if (a) return a;\n", { mode: "check" }).findings,
  ).not.toContainEqual(expect.objectContaining({ rule: "error" }));
});

test("editorconfig lookup stops at the project root", () => {
  for (const marker of [".git", ".hg"]) {
    const root = dirWith({
      "package.json": '{ "devDependencies": { "prettier": "3" } }',
      ".editorconfig": "[*]\nmax_line_length = 120\n",
      "child/.editorconfig": "[*]\ntab_width = 7\n",
    });

    mkdirSync(join(root, "child", marker));

    expect(formatterWidth(join(root, "child/a.ts"))).toMatchObject({
      columns: 80,
      tab: 7,
      source: { kind: "default", formatter: "prettier" },
    });
  }
});

test("unknown formatter values and overrides leave the config unread", () => {
  for (const config of [
    "const w = Number(process.env.W ?? 120); export default { printWidth: w };",
    "const w = Number(process.env.W ?? 2); export default { tabWidth: w };",
    "const w = Number(process.env.W ?? 2); export default { overrides: w };",
    "const w = process.env.W; export default { overrides: [{ files: w, options: { printWidth: 90 } }] };",
    'const w = Number(process.env.W ?? 90); export default { overrides: [{ files: "*.ts", options: { printWidth: w } }] };',
  ]) {
    const root = dirWith({
      ".prettierrc": '{ "printWidth": 120 }',
      "child/prettier.config.mjs": config,
    });

    expect(formatterWidth(join(root, "child/a.ts"))).toMatchObject({
      columns: 120,
      unread: [join(root, "child/prettier.config.mjs")],
    });
  }
});

test("formatter width caches the resolved real path", () => {
  const root = dirWith({
    ".prettierrc":
      '{ "printWidth": 120, "overrides": [{ "files": "src/*.ts", "options": { "printWidth": 90 } }] }',
  });

  mkdirSync(join(root, "src"));
  symlinkSync(root, `${root}-link`);
  const first = formatterWidth(join(root, "src/a.ts"));
  expect(first.columns).toBe(90);
  expect(formatterWidth(join(`${root}-link`, "src/a.ts"))).toBe(first);
});

test("editorconfig parsing is shared across target files", () => {
  const root = dirWith({
    "package.json": '{ "devDependencies": { "prettier": "3" } }',
    ".editorconfig": "[*]\nmax_line_length = 110\n",
  });

  expect(formatterWidth(join(root, "a.ts")).columns).toBe(110);

  writeFileSync(join(root, ".editorconfig"), "[*]\nmax_line_length = 120\n");
  expect(formatterWidth(join(root, "b.ts")).columns).toBe(110);
});

test("a comment after a guard header prevents joining", () => {
  const root = dirWith({ ".prettierrc": '{ "printWidth": 120 }' });
  const text =
    "function f(a: boolean) {\n  if (a) // TODO (temporary)\n    return a;\n  if (a) return a;\n}\n";

  expect(formatText(join(root, "a.ts"), text, { mode: "fix" }).fixed).toContain(
    "return a;\n\n  if (a)",
  );
});

test("corpus judge accepts settings resolved once for a file", () => {
  const root = dirWith({ ".prettierrc": '{ "printWidth": 120 }' });
  const path = join(root, "a.ts");
  const settings = stepSettings(path, { braces: "off" });
  const verdict = judge(
    path,
    "function f(a: boolean) {\n  if (a) return a;\n}\n",
    settings.keepBraces,
    undefined,
    settings,
  );

  expect(settings.width.columns).toBe(120);
  expect(verdict.kind).toBe("judged");
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

test("unread formatter configs are treated as absent", () => {
  const root = dirWith({
    "package.json": '{ "devDependencies": { "oxfmt": "1" } }',
    "child/.oxfmtrc.json": "{ invalid",
  });

  expect(formatterWidth(join(root, "child/x.ts"))).toMatchObject({
    columns: 100,
    source: { kind: "default", formatter: "oxfmt" },
    unread: [join(root, "child/.oxfmtrc.json")],
  });
});

test("a joined guard counts indentation tabs at the formatter tab width", () => {
  const text =
    "function settle(reservation: Reservation) {\n\tif (reservation.active)\n\t\treturn reservation.confirm();\n\tif (reservation.pending)\n\t\treturn reservation.cancel();\n}\n";

  const narrow = dirWith({ ".prettierrc": '{ "printWidth": 58, "tabWidth": 4 }' });
  const wide = dirWith({ ".prettierrc": '{ "printWidth": 58, "tabWidth": 8 }' });

  expect(formatText(join(narrow, "x.ts"), text, { mode: "fix" }).fixed).toBeUndefined();
  expect(formatText(join(wide, "x.ts"), text, { mode: "fix" }).fixed).toContain(
    "return reservation.confirm();\n\n\tif",
  );
});

function sharedConfig(levels: number, next: (previous: string) => string): string {
  const lines = ['const c0 = { rules: { curly: "off" } };'];
  for (let level = 1; level <= levels; level++)
    lines.push(`const c${level} = ${next(`c${level - 1}`)};`);

  return `${lines.join("\n")}\nexport default c${levels};\n`;
}

test("shared flat config layers finish within one second", () => {
  const dir = dirWith({
    "eslint.config.js": sharedConfig(30, (previous) => `[${previous}, ${previous}]`),
    "x.ts": "if (ok) {\n  run();\n}\n",
  });

  const start = performance.now();
  const decisions = braceDecisions(dir, ".ts");
  expect(performance.now() - start).toBeLessThan(1000);
  expect(decisions).toEqual([
    { family: "flat", setting: "off", files: [join(dir, "eslint.config.js")] },
  ]);

  const checked = run({ cwd: dir }, "--check", "x.ts");
  expect(checked.stdout).toContain(" braces ");
  expect(checked.stderr).toBe("");

  expect(bracesEnforced(dir, ".ts")).toBe(false);
});

test("distinct flat scopes exhaust the expansion budget", () => {
  const dir = dirWith({
    "eslint.config.js": sharedConfig(
      30,
      (previous) =>
        `[{ files: ["**/*.ts"], extends: [${previous}] }, { files: ["**/*.js"], extends: [${previous}] }]`,
    ),
    "x.ts": "if (ok) {\n  run();\n}\n",
  });

  const start = performance.now();
  const decisions = braceDecisions(dir, ".ts");
  expect(performance.now() - start).toBeLessThan(1000);
  expect(decisions).toEqual([
    { family: "flat", setting: "unknown", files: [join(dir, "eslint.config.js")] },
  ]);

  const checked = run({ cwd: dir }, "--check", "x.ts");
  expect(checked.stdout).not.toContain(" braces ");
  expect(checked.stderr).toBe(
    "stanza: could not tell whether eslint.config.js enforces braces, so braces stay; pass --braces or --no-braces to settle it\n",
  );
});

test("shared oxlint extends finish within one second", () => {
  const dir = dirWith({
    "oxlint.config.ts": sharedConfig(30, (previous) => `{ extends: [${previous}, ${previous}] }`),
    "x.ts": "if (ok) {\n  run();\n}\n",
  });

  const start = performance.now();
  const decisions = braceDecisions(dir, ".ts");
  expect(performance.now() - start).toBeLessThan(1000);
  expect(decisions).toEqual([
    { family: "oxlint", setting: "off", files: [join(dir, "oxlint.config.ts")] },
  ]);

  const checked = run({ cwd: dir }, "--check", "x.ts");
  expect(checked.stdout).toContain(" braces ");
  expect(checked.stderr).toBe("");

  expect(bracesEnforced(dir, ".ts")).toBe(false);
});

test("the decision names the config file that decided it", () => {
  const dir = realpathSync(
    dirWith({
      "eslint.config.js": 'export default [{ rules: { curly: "error" } }];',
      "app/eslint.config.js": 'export default [{ rules: { curly: "off" } }];',
    }),
  );

  expect(braceDecisions(join(dir, "app"))).toEqual([
    { family: "flat", setting: "on", files: [join(dir, "eslint.config.js")] },
  ]);
});

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

test("oxlint rules and categories control braces", () => {
  expect(
    bracesEnforced(dirWith({ ".oxlintrc.json": '{ "rules": { "eslint/curly": "error" } }' })),
  ).toBe(true);

  expect(
    bracesEnforced(dirWith({ ".oxlintrc.json": '{ "categories": { "style": "warn" } }' })),
  ).toBe(true);

  expect(
    bracesEnforced(
      dirWith({
        ".oxlintrc.json": '{ "categories": { "style": "error" }, "rules": { "curly": "off" } }',
      }),
    ),
  ).toBe(false);

  expect(bracesEnforced(dirWith({ ".oxlintrc.json": '{ "rules": { "curly": "allow" } }' }))).toBe(
    false,
  );
});

test("oxlint overrides use legacy file globs", () => {
  const root = dirWith({
    ".oxlintrc.json":
      '{ "overrides": [{ "files": ["legacy/**"], "rules": { "curly": "error" } }] }',
  });

  expect(bracesEnforced(join(root, "legacy"), ".ts")).toBe(true);
  expect(bracesEnforced(root, ".ts")).toBe(false);
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

test("the nearest oxlint config decides alone", () => {
  const root = dirWith({
    ".oxlintrc.json": '{ "rules": { "curly": "error" } }',
    "nested/.oxlintrc.json": "{}",
  });

  expect(bracesEnforced(join(root, "nested"))).toBe(false);
});

test("oxlint extends apply categories, rules, then overrides", () => {
  for (const [base, own, expected] of [
    ['{ "rules": { "curly": "off" } }', '{ "categories": { "style": "warn" } }', false],
    ['{ "rules": { "curly": "warn" } }', '{ "categories": { "style": "off" } }', true],
  ] as const) {
    const root = dirWith({
      "base.json": base,
      ".oxlintrc.json": `{ "extends": ["./base.json"], ${own.slice(1)}`,
    });

    expect(bracesEnforced(root)).toBe(expected);
  }

  for (const [entries, expected] of [
    ['"./on.json", "./off.json"', false],
    ['"./off.json", "./on.json"', true],
  ] as const)
    expect(
      bracesEnforced(
        dirWith({
          "on.json": '{ "rules": { "curly": "error" } }',
          "off.json": '{ "rules": { "curly": "off" } }',
          ".oxlintrc.json": `{ "extends": [${entries}] }`,
        }),
      ),
    ).toBe(expected);
});

test("multiple oxlint configs keep braces and name every config", () => {
  const dir = realpathSync(
    dirWith({ ".oxlintrc.json": "{}", "oxlint.config.ts": "export default {};" }),
  );

  expect(bracesEnforced(dir)).toBe(true);
  expect(braceDecisions(dir)).toEqual([
    {
      family: "oxlint",
      setting: "unknown",
      files: [join(dir, ".oxlintrc.json"), join(dir, "oxlint.config.ts")],
    },
  ]);
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

test("a package.json with a byte order mark is read like ESLint reads it", () => {
  expect(braceDecisions(dirWith({ "package.json": '﻿{ "name": "x" }' }))).toEqual([]);
  expect(
    bracesEnforced(
      dirWith({ "package.json": '﻿{ "eslintConfig": { "rules": { "curly": "off" } } }' }),
    ),
  ).toBe(false);
});

test("legacy eslint prefers .eslintrc.json over package.json", () => {
  const dir = dirWith({
    ".eslintrc.json": '{ "rules": { "curly": "off" } }',
    "package.json": '{ "eslintConfig": { "rules": { "curly": "error" } } }',
  });

  expect(bracesEnforced(dir)).toBe(false);
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

test("symlinked configs retain their discovered anchors", () => {
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

test("unsupported glob syntax remains uncertain in includes and exclusions", () => {
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

test("biome double slash inherits the nearest ancestor at its own anchor", () => {
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

test("a config nested in an unknown call argument escapes", () => {
  for (const call of [
    "weaken({ config });",
    "weaken([config]);",
    "weaken({ ...config });",
    "weaken({ inner: { config } });",
    "weaken(() => config);",
    "weaken(config);",
  ]) {
    const root = dirWith({
      "eslint.config.js": `const config = { rules: { curly: "off" } }; ${call} export default [config];`,
    });

    expect(bracesEnforced(root)).toBe(true);
  }

  for (const call of [
    "weaken((config) => config);",
    "weaken(function config() { return config; });",
    "weaken(() => { const config = {}; return config; });",
    "weaken(() => { if (true) { var config = {}; } return config; });",
    "weaken(class config { method() { return config; } });",
  ]) {
    const root = dirWith({
      "eslint.config.js": `const config = { rules: { curly: "off" } }; ${call} export default [config];`,
    });

    expect(bracesEnforced(root)).toBe(false);
  }

  const root = dirWith({
    "eslint.config.js":
      'import { defineConfig } from "eslint/config"; const config = { rules: { curly: "off" } }; export default defineConfig([config]);',
  });

  expect(bracesEnforced(root)).toBe(false);
});

test("only imported table helpers can flatten or ignore arguments", () => {
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

test("namespace collisions, cycles, oversized arrays and thrown reads are safe", () => {
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

test("YAML rule mentions outside the matched line remain unknown", () => {
  for (const yaml of [
    "rules: { curly: error }\n",
    "rules:\n  curly: off\noverrides: [{ rules: { curly: error } }]\n",
    "rules:\n  curly: off\nother:\n  curly: error\n",
  ])
    expect(bracesEnforced(dirWith({ ".eslintrc.yml": yaml }))).toBe(true);
});

test("typescript presets are arrays and recommended JS rules are spreadable", () => {
  for (const script of [
    'import ts from "typescript-eslint"; export default [...ts.configs.recommended];',
    'import ts from "typescript-eslint"; export default ts.config({ extends: [...ts.configs.strict] });',
    'import ts from "typescript-eslint"; export default [{ extends: ts.configs.anything }];',
    'import js from "@eslint/js"; export default [{ rules: { ...js.configs.recommended.rules } }];',
    'import js from "@eslint/js"; import ts from "typescript-eslint"; import prettier from "eslint-config-prettier"; export default ts.config(js.configs.recommended, ...ts.configs.recommended, prettier);',
  ])
    expect(bracesEnforced(dirWith({ "eslint.config.js": script }))).toBe(false);
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

test("60 scoped config objects fold across 700 directories within one second", () => {
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

test("family-specific rule reads work without redundant rule parameters", () => {
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
