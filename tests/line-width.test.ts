import { expect, test } from "bun:test";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { run, scratch } from "./support.ts";

function dirWith(files: Record<string, string>): string {
  const dir = scratch("config");
  for (const [name, text] of Object.entries(files)) {
    mkdirSync(dirname(join(dir, name)), { recursive: true });
    writeFileSync(join(dir, name), text);
  }

  return dir;
}

test("fix takes an .editorconfig width only in the governing formatter's spelling", () => {
  const guard = Buffer.from(`function f(a) {\n  if (a) {\n    log("${"x".repeat(65)}");\n  }\n}\n`);
  const joined = `  if (a) log("${"x".repeat(65)}");\n`;
  const rows: [string, string, boolean, string][] = [
    [".prettierrc.json", "120", true, ""],
    [".prettierrc.json", "0x78", false, ""],
    [".prettierrc.json", "+120", false, ""],
    [".oxfmtrc.json", "+120", true, ""],
    [".oxfmtrc.json", "1.2e2", true, ""],
    [".oxfmtrc.json", "80", false, ""],
    [
      ".oxfmtrc.json",
      "321",
      true,
      "stanza: cannot read the line width from .editorconfig, so the next source sets it\n",
    ],
  ];

  for (const [config, value, joins, stderr] of rows) {
    const cwd = dirWith({
      [config]: "{}",
      ".editorconfig": `root = true\n[*]\nmax_line_length = ${value}\n`,
    });

    const result = run({ cwd, stdin: guard }, "--fix", "--braces", "--stdin", "a.ts");
    expect([config, value, result.code, result.stdout.includes(joined), result.stderr]).toEqual([
      config,
      value,
      0,
      joins,
      stderr,
    ]);
  }
});

test("fix joins any guard under an .editorconfig max_line_length = off only for Prettier", () => {
  const rows: [string, string, number, boolean][] = [
    [".prettierrc.json", "off", 65, true],
    [".prettierrc.json", "off", 300, true],
    [".prettierrc.json", '"off"', 300, true],
    [".prettierrc.json", "OFF", 65, false],
    [".oxfmtrc.json", "off", 65, true],
    [".oxfmtrc.json", "off", 300, false],
  ];

  for (const [config, value, length, joins] of rows) {
    const cwd = dirWith({
      [config]: "{}",
      ".editorconfig": `root = true\n[*]\nmax_line_length = ${value}\n`,
    });

    const text = `function f(a) {\n  if (a) {\n    log("${"x".repeat(length)}");\n  }\n}\n`;
    const result = run({ cwd, stdin: Buffer.from(text) }, "--fix", "--braces", "--stdin", "a.ts");
    expect([config, value, length, result.code, result.stdout.includes("  if (a) log(")]).toEqual([
      config,
      value,
      length,
      0,
      joins,
    ]);
  }
});

test("fix skips a package.json that is not strict JSON, as Prettier does", () => {
  const guard = Buffer.from(`function f(a) {\n  if (a) {\n    log("${"x".repeat(65)}");\n  }\n}\n`);
  const sibling = { ".prettierrc.json": '{"printWidth":100}' };
  const cases: [Record<string, string>, boolean][] = [
    [{ "package.json": '{"prettier":{"printWidth":94},}' }, false],
    [{ "package.json": '{/* comment */"prettier":{"printWidth":94}}' }, false],
    [{ "package.json": "{'prettier':{'printWidth':94}}" }, false],
    [{ "package.json": '{"prettier":{"printWidth":94},}', ...sibling }, true],
    [{ "package.json": '{"prettier":{"printWidth":94}}' }, true],
  ];

  for (const [files, joins] of cases) {
    const result = run(
      { cwd: dirWith(files), stdin: guard },
      "--fix",
      "--braces",
      "--stdin",
      "a.ts",
    );

    expect([files, result.code, result.stdout.includes("  if (a) log("), result.stderr]).toEqual([
      files,
      0,
      joins,
      "",
    ]);
  }
});

test("fix reads no oxfmt or Biome width above 320, as those formatters refuse it", () => {
  const guard = Buffer.from(`function f(a) {\n  if (a) {\n    log("${"x".repeat(65)}");\n  }\n}\n`);
  const unread = (config: string) =>
    `stanza: cannot read the line width from ${config}, so the next source sets it\n`;

  const prettier = { ".prettierrc.json": '{"printWidth":120}' };
  const rows: [Record<string, string>, boolean, string][] = [
    [{ ".oxfmtrc.json": '{"printWidth":321}' }, false, unread(".oxfmtrc.json")],
    [{ "biome.json": '{"formatter":{"lineWidth":321}}' }, false, unread("biome.json")],
    [{ ".oxfmtrc.json": '{"printWidth":320}' }, true, ""],
    [{ "biome.json": '{"formatter":{"lineWidth":320}}' }, true, ""],
    [{ ".oxfmtrc.json": '{"printWidth":321}', ...prettier }, true, unread(".oxfmtrc.json")],
    [{ "biome.json": '{"formatter":{"lineWidth":321}}', ...prettier }, true, unread("biome.json")],
    [
      { "biome.json": '{"javascript":{"formatter":{"lineWidth":321}}}' },
      false,
      unread("biome.json"),
    ],
    [
      {
        ".oxfmtrc.json":
          '{"printWidth":120,"overrides":[{"files":["*.md"],"options":{"printWidth":321}}]}',
      },
      false,
      unread(".oxfmtrc.json"),
    ],
    [
      {
        "biome.json":
          '{"formatter":{"lineWidth":120},"overrides":[{"includes":["*.md"],"formatter":{"lineWidth":321}}]}',
      },
      false,
      unread("biome.json"),
    ],
    [{ ".prettierrc.json": '{"printWidth":321}' }, true, ""],
  ];

  for (const [files, joins, stderr] of rows) {
    const result = run(
      { cwd: dirWith(files), stdin: guard },
      "--fix",
      "--braces",
      "--stdin",
      "a.ts",
    );

    expect([files, result.code, result.stdout.includes("  if (a) log("), result.stderr]).toEqual([
      files,
      0,
      joins,
      stderr,
    ]);
  }
});

test("fix takes the width of a shared Prettier or Biome config", () => {
  const guard = Buffer.from(`function f(a) {\n  if (a) {\n    log("${"x".repeat(50)}");\n  }\n}\n`);
  const prettier = {
    "node_modules/@org/prettier-config/package.json": '{"main":"index.json"}',
    "node_modules/@org/prettier-config/index.json": '{"printWidth":60}',
  };

  const trees: Record<string, string>[] = [
    { ...prettier, "package.json": '{"prettier":"@org/prettier-config"}' },
    { ...prettier, ".prettierrc": '"@org/prettier-config"' },
    {
      "node_modules/@org/biome-config/package.json": '{"exports":{"./biome":"./biome.json"}}',
      "node_modules/@org/biome-config/biome.json": '{"formatter":{"lineWidth":60}}',
      "biome.json": '{"extends":["@org/biome-config/biome"]}',
    },
  ];

  for (const files of trees) {
    const result = run(
      { cwd: dirWith(files), stdin: guard },
      "--fix",
      "--braces",
      "--stdin",
      "a.ts",
    );

    expect([result.code, result.stdout.includes("  if (a)\n"), result.stderr]).toEqual([
      0,
      true,
      "",
    ]);
  }
});

test("fix reads a Biome extends chain through ten files and no further", () => {
  const guard = Buffer.from(`function f(a) {\n  if (a) {\n    log("${"x".repeat(65)}");\n  }\n}\n`);
  const chain = (files: number) => {
    const tree: Record<string, string> = { "biome.json": '{"extends":["./c1.json"]}' };
    for (let index = 1; index < files; index++)
      tree[`c${index}.json`] = `{"extends":["./c${index + 1}.json"]}`;

    tree[`c${files}.json`] = '{"formatter":{"lineWidth":90}}';
    return dirWith(tree);
  };

  const outcomes = [10, 11].map((files) => {
    const result = run({ cwd: chain(files), stdin: guard }, "--fix", "--braces", "--stdin", "a.ts");
    return [result.code, result.stdout.includes("  if (a)\n"), result.stderr];
  });

  expect(outcomes).toEqual([
    [0, false, ""],
    [0, true, "stanza: cannot read the line width from biome.json, so the next source sets it\n"],
  ]);
});

test("fix applies .editorconfig sections where each formatter does", () => {
  const guard = Buffer.from(`function f(a) {\n  if (a) {\n    log("${"x".repeat(65)}");\n  }\n}\n`);
  const joined = `  if (a) log("${"x".repeat(65)}");\n`;
  const unread =
    "stanza: cannot read the line width from .editorconfig, so the next source sets it\n";

  const rows: [string, string, string, boolean, string][] = [
    [".prettierrc.json", "a\\ b.ts", "a b.ts", false, ""],
    [".prettierrc.json", "{a}.ts", "a.ts", true, ""],
    [".oxfmtrc.json", "a.ts", "src/a.ts", true, unread],
    [".oxfmtrc.json", "*.ts", "src/a.ts", false, ""],
  ];

  for (const [config, section, path, joins, stderr] of rows) {
    const cwd = dirWith({
      [config]: "{}",
      ".editorconfig": `root = true\n[*]\nmax_line_length = 120\n[${section}]\nmax_line_length = 40\n`,
    });

    const result = run({ cwd, stdin: guard }, "--fix", "--braces", "--stdin", path);
    expect([section, result.code, result.stdout.includes(joined), result.stderr]).toEqual([
      section,
      0,
      joins,
      stderr,
    ]);
  }
});

const readyGuard =
  'function f() {\n  if (ready) {\n    return someFunctionWithALongNameThatTakesSpace("1234567890123456789012345678901234567890");\n  }\n}\n';

const readyJoined = '  if (ready) return someFunctionWithALongNameThatTakesSpace("';

function fixedInPlace(cwd: string, path: string): { code: number; stderr: string; text: string } {
  const result = run({ cwd, env: { ...process.env, HOME: cwd } }, "--fix", path);
  return { code: result.code, stderr: result.stderr, text: readFileSync(join(cwd, path), "utf8") };
}

test("config comments keep Unicode separators inside the comment", () => {
  for (const separator of ["\u2028", "\u2029"]) {
    const cwd = dirWith({
      ".prettierrc.toml": `printWidth = 120 # x${separator}\n`,
      "a.ts": readyGuard,
    });

    const { code, stderr, text } = fixedInPlace(cwd, "a.ts");
    expect([separator, code, stderr, text.includes(readyJoined)]).toEqual([
      separator,
      0,
      "rewrote 1 file, no findings left\n",
      true,
    ]);
  }
});

test("Unicode section names do not become the preamble", () => {
  for (const separator of ["\u2028", "\u2029"]) {
    const matching = `a${separator}b.ts`;
    const cwd = dirWith({
      ".prettierrc.json": "{}",
      ".editorconfig": `[${matching}]\nmax_line_length = 120\n`,
      [matching]: readyGuard,
      "other.ts": readyGuard,
    });

    const joined = fixedInPlace(cwd, matching);
    expect([separator, joined.code, joined.text.includes(readyJoined)]).toEqual([
      separator,
      0,
      true,
    ]);

    const other = fixedInPlace(cwd, "other.ts");
    expect([separator, other.code, other.text]).toEqual([
      separator,
      0,
      'function f() {\n  if (ready)\n    return someFunctionWithALongNameThatTakesSpace("1234567890123456789012345678901234567890");\n}\n',
    ]);
  }
});

test("fix reads .editorconfig where each formatter finds it", () => {
  const guard = Buffer.from(`function f(a) {\n  if (a) {\n    log("${"x".repeat(65)}");\n  }\n}\n`);
  const joined = `  if (a) log("${"x".repeat(65)}");\n`;
  const joins = (cwd: string, path: string) => {
    const result = run({ cwd, stdin: guard }, "--fix", "--braces", "--stdin", path);
    expect([result.code, result.stderr]).toEqual([0, ""]);
    return result.stdout.includes(joined);
  };

  const biome = dirWith({
    "biome.json": '{"formatter":{"useEditorconfig":true}}',
    ".editorconfig": "root = true\n[*]\nmax_line_length = 120\ntab_width = 8\nindent_size = 2\n",
  });

  const wideBiome = dirWith({
    "biome.json": '{"formatter":{"useEditorconfig":true,"lineWidth":120}}',
    ".editorconfig": "root = true\n[*]\nmax_line_length = 40\n",
  });

  expect([joins(biome, "a.ts"), joins(wideBiome, "a.ts")]).toEqual([false, true]);

  const oxfmtConfigs: Record<string, string>[] = [
    { ".oxfmtrc.json": "{}" },
    { "package.json": '{ "devDependencies": { "oxfmt": "1" } }' },
  ];

  for (const config of oxfmtConfigs) {
    const anchored = dirWith({
      ...config,
      ".editorconfig": "root = true\n[*]\nmax_line_length = 40\n",
      "child/.editorconfig": "[*]\ntab_width = 4\n",
    });

    const nearest = dirWith({ ...config, "child/.editorconfig": "[*]\nmax_line_length = 40\n" });
    expect([
      joins(anchored, "child/a.ts"),
      joins(join(anchored, "child"), "a.ts"),
      joins(nearest, "child/a.ts"),
      joins(join(nearest, "child"), "a.ts"),
    ]).toEqual([false, false, true, true]);
  }

  const hg = dirWith({
    ".prettierrc.json": "{}",
    ".editorconfig": "[*]\nmax_line_length = 120\n",
    "p/.hg": "",
  });

  expect(joins(hg, "p/a.ts")).toBe(true);
});
