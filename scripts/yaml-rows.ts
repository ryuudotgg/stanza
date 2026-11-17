import { join } from "node:path";
import { parseDocument } from "yaml";
import yamlPackage from "yaml/package.json";

const YAML = "2.9.1";
const root = join(import.meta.dir, "..");
const table = join(
  root,
  "crates",
  "stanza",
  "src",
  "languages",
  "javascript",
  "config",
  "yaml-rows.json",
);

const TREE: [string, string][] = [
  ["block-lint", "rules:\n  curly: off\n"],
  ["quoted-rule", '"rules":\n  "curly": "error"\n'],
  [
    "override-extends",
    '"rules":\n  "curly": "off"\n"overrides":\n  - "files": ["*.ts"]\n    "extends": ["eslint:all"]\n',
  ],
  [
    "block-width",
    'printWidth: 100\noverrides:\n  - files: "*.md"\n    options:\n      printWidth: 40\n',
  ],
  ["flow-maps", '{rules: {curly: [error, all]}, root: true, extends: ["eslint:all"]}\n'],
  [
    "flow-width",
    '{printWidth: 100, tabWidth: 4, overrides: [{files: "*.ts", options: {printWidth: 40}}]}\n',
  ],
  ["block-sequence", 'extends:\n  - "./base.json"\n  - "eslint:all"\n'],
  ["document-start", "---\nprintWidth: 100 # width\n"],
  ["scalars", "a: true\nb: false\nc: null\nd: ~\ne:\nf: -12.5e2\ng: 100\nh: off\ni: 2026-11-16\n"],
  ["quoted-scalars", "'a': 'it''s # data'\n\"b\": \"line\\nnext\\u0061\"\nc: a#b\n"],
  ["empty-collections", "a: {}\nb: []\n"],
  ["crlf", 'printWidth: 120 # x\r\nx: "a b"\r\n'],
  ["comment-ls", "printWidth: 120 # x \n"],
  ["comment-ps", "printWidth: 120 # x \n"],
  ["quoted-ls", 'printWidth: 120\nx: "a b"\n'],
  ["quoted-ps", 'printWidth: 120\nx: "a b"\n'],
  ["root-comment", "root: true # stop\nrules: {curly: off}\n"],
  ["package-width", "prettier:\n  printWidth: 60\n"],
];

const UNKNOWN: [string, string][] = [
  ["anchor-alias", "base: &b 100\nprintWidth: *b\n"],
  ["unrelated-anchor", "printWidth: 100\nextra: &b [1, 2]\n"],
  ["tag", 'printWidth: !!int "100"\n'],
  ["merge-key", "extra: {<<: {a: 1}}\nprintWidth: 100\n"],
  ["documents", "printWidth: 100\n---\nprintWidth: 40\n"],
  ["block-scalar", "printWidth: 100\nextra: |\n  data\n"],
  ["folded-scalar", "printWidth: 100\nextra: >\n  data\n"],
  ["indent-tab", "rules:\n\tcurly: error\n"],
  ["bad-indent", "rules:\n  curly: error\n x: true\n"],
  ["duplicate-key", "printWidth: 100\nprintWidth: 40\n"],
  ["complex-key", "? [a, b]\n: 1\n"],
  ["numeric-key", "{1: value}\n"],
  ["directive", "%YAML 1.2\n---\nprintWidth: 100\n"],
  ["document-end", "printWidth: 100\n...\n"],
  ["multiline-plain", "extra: hello\n  world\nprintWidth: 100\n"],
  ["multiline-quoted", 'extra: "hello\n  world"\nprintWidth: 100\n'],
  ["lone-cr", "printWidth: 100\rextra: true\r"],
  ["nonfinite", "printWidth: .inf\n"],
  ["hex-number", "printWidth: 0x64\n"],
  ["leading-zero", "printWidth: 0100\n"],
  ["uppercase-bool", "root: TRUE\n"],
  ["tight-flow-colon", '{rules:{curly:"off"}}\n'],
  ["trailing-junk", "printWidth: [100\n"],
];

if (yamlPackage.version !== YAML) {
  console.error(
    `yaml-rows: the table records yaml ${YAML}; node_modules holds ${yamlPackage.version}`,
  );

  process.exit(2);
}

function parsed(input: string): unknown {
  const document = parseDocument(input, { logLevel: "error" });
  const error = document.errors[0];
  return error ? { error: error.code } : { tree: document.toJS() };
}

const rows = [
  ...TREE.map(([name, input]) => ({ name, input, reader: "tree", parser: parsed(input) })),
  ...UNKNOWN.map(([name, input]) => ({ name, input, reader: "unknown", parser: parsed(input) })),
];

const json = JSON.stringify(
  rows,
  (_, item: unknown) =>
    typeof item === "number" && !Number.isFinite(item) ? { $number: String(item) } : item,
  2,
);

await Bun.write(table, `${json}\n`);

const formatted = Bun.spawnSync(
  [join(root, "node_modules", ".bin", "oxfmt"), "--disable-nested-config", table],
  { stdout: "inherit", stderr: "inherit" },
);

if (formatted.exitCode !== 0) process.exit(formatted.exitCode ?? 1);

console.log(`yaml-rows: ${TREE.length} tree rows, ${UNKNOWN.length} unknown rows`);
