import { readFileSync } from "node:fs";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
const BUN = readFileSync(join(root, ".bun-version"), "utf8").trim();
const config = join(root, "crates", "stanza", "src", "languages", "javascript", "config");

const PATTERNS = [
  "*",
  "**",
  "*.ts",
  "*.test.ts",
  "**/*.ts",
  "**/*.test.ts",
  "src/**",
  "src/**/*",
  "src/**/*.ts",
  "src/*.ts",
  "**/src/**",
  "**/dist/**",
  "dist",
  "dist/**",
  "a/**/b",
  "a/**/b/**",
  "a**b",
  "**.ts",
  "**/a/**",
  "**/a",
  "*/",
  "src/",
  "./a.ts",
  "/a.ts",
  "{a,b}.ts",
  "{a,{b,c}}.ts",
  "*.{js,ts}",
  "**/*.{js,jsx,ts,tsx}",
  "{src,test}/**",
  "{**/a,**/b}",
  "{,a}b",
  "*{a,b}",
  "{a,b}*",
  "{a,b",
  "{a",
  "a}",
  "{a}",
  "a,b",
  "{a,b}{c,d}",
  "{a,[b,c]}",
  "{a\\,b,c}",
  "[abc].ts",
  "[!abc].ts",
  "[^a].ts",
  "[a-c].ts",
  "[a-].ts",
  "[]].ts",
  "[!]].ts",
  "[",
  "[a",
  "*.[jt]s",
  "[é].ts",
  "[à-ü].ts",
  "\\*.ts",
  "\\[a].ts",
  "\\n",
  "a\\",
  "?",
  "?.ts",
  "a?c",
  "!*.ts",
  "!!*.ts",
  "!src/**",
  "é.ts",
  "*é.ts",
  "a/*/b",
  "a/**",
  "**/",
  "x/**/",
  "**/**/*.ts",
  "a/**/**/b",
  "",
  "{**}",
  "{a,**}",
  "{src/**,lib/**}",
  "{src/**,test/**}",
  "!{dist/**,build/**}",
  "{",
];

const PATHS = [
  "",
  "a",
  "a.ts",
  ".a.ts",
  "b.ts",
  "c.ts",
  "d.ts",
  "-.ts",
  "].ts",
  "*.ts",
  "[a].ts",
  "x.test.ts",
  "x.js",
  "x.jsx",
  "abc",
  "a/c",
  "ab",
  "b",
  "ac",
  "ad",
  "bc",
  "a,b",
  "{a",
  "a}",
  "\n",
  "a\\",
  "axyb",
  "ax/yb",
  "a/b",
  "a/",
  "a//b",
  "a/x/b",
  "a/x/y/b",
  "a/b/c",
  "a/b/c.ts",
  "src",
  "src/",
  "src/a.ts",
  "src/a/b",
  "src/a/b.ts",
  "test/a.ts",
  "dist",
  "dist/a.js",
  "dist/a/b.ts",
  "lib/dist/a.ts",
  "x/a/y",
  "x/a",
  "x/",
  "x/y/",
  "é.ts",
  "ü.ts",
  "aé.ts",
  "😀.ts",
];

if (Bun.version !== BUN) {
  console.error(
    `width-tables: run this with Bun ${BUN}, the version CI runs; this is ${Bun.version}`,
  );

  process.exit(2);
}

function seeded(seed: number): () => number {
  let state = seed;
  return () => {
    state = (state + 0x6d2b79f5) | 0;

    let mixed = Math.imul(state ^ (state >>> 15), state | 1);
    mixed ^= mixed + Math.imul(mixed ^ (mixed >>> 7), mixed | 61);
    return ((mixed ^ (mixed >>> 14)) >>> 0) / 4294967296;
  };
}

const random = seeded(238);
const pick = <T>(items: readonly T[]): T => items[Math.floor(random() * items.length)]!;
const chance = (odds: number) => random() < odds;
const LETTERS = ["a", "b", "c", ".", "é", "😀"];

type Part = { glob: string; path: () => string };

function literal(): Part {
  const text = pick(LETTERS);
  return { glob: text, path: () => text };
}

function part(depth: number): Part {
  const kind = random();
  if (kind < 0.3) return literal();

  if (kind < 0.42)
    return {
      glob: "*",
      path: () => Array.from({ length: Math.floor(random() * 3) }, () => pick(LETTERS)).join(""),
    };

  if (kind < 0.5) return { glob: "?", path: () => pick(LETTERS) };

  if (kind < 0.58) {
    const [glob, member] = pick([
      ["[ab]", "a"],
      ["[!a]", "b"],
      ["[^.]", "c"],
      ["[a-c]", "b"],
      ["[]a]", "]"],
      ["[a-]", "-"],
    ]);

    return { glob, path: () => (chance(0.8) ? member : pick(LETTERS)) };
  }

  if (kind < 0.66) return { glob: "/", path: () => "/" };
  if (kind < 0.72) return { glob: "**/", path: () => pick(["", "a/", "a/b/", ".c/"]) };

  if (kind < 0.9 && depth < 3) {
    const alternatives = Array.from({ length: 1 + Math.floor(random() * 3) }, () =>
      sequence(depth + 1, 2),
    );

    const closed = chance(0.85);
    const glob = `{${alternatives.map((it) => it.glob).join(",")}${closed ? "}" : ""}`;
    return { glob, path: () => (closed ? pick(alternatives).path() : glob) };
  }

  if (kind < 0.94 && depth < 3) {
    const alternatives = Array.from({ length: 1 + Math.floor(random() * 3) }, () =>
      sequence(depth + 1, 2),
    );

    const glob = `${pick(["@", "?", "*", "+", "!"])}(${alternatives.map((it) => it.glob).join("|")})`;
    return { glob, path: () => pick(alternatives).path() };
  }

  if (kind < 0.97) {
    const [glob, values] = pick([
      ["{1..3}", ["1", "2", "3"]],
      ["{01..10..3}", ["01", "04", "07", "10"]],
      ["{a..c}", ["a", "b", "c"]],
    ] as const);

    return { glob, path: () => pick(values) };
  }

  return { glob: "\\*", path: () => "*" };
}

function sequence(depth: number, longest: number): Part {
  const parts = Array.from({ length: Math.floor(random() * (longest + 1)) }, () => part(depth));
  return {
    glob: parts.map((it) => it.glob).join(""),
    path: () => parts.map((it) => it.path()).join(""),
  };
}

function mutated(path: string): string {
  if (path === "" || chance(0.5)) return path + pick(["", "a", "/", "."]);

  const characters = [...path];
  characters[Math.floor(random() * characters.length)] = pick(LETTERS);
  return characters.join("");
}

const ALTERNATIVES = 110;
const HOSTILE: [string, string][] = [
  [`{${"b,".repeat(ALTERNATIVES)}bb}{${Array(ALTERNATIVES).fill("b").join(",")}}z`, "bbbz"],
  ["{a}".repeat(11), "a".repeat(11)],
  [`${"{".repeat(11)}a${"}".repeat(11)}`, "a"],
];

const GENERATED_PAIRS = 2400;
const pairs = new Map<string, [string, string]>();
const add = (pattern: string, path: string) =>
  pairs.set(JSON.stringify([pattern, path]), [pattern, path]);

for (const pattern of PATTERNS) for (const path of PATHS) add(pattern, path);

const grid = pairs.size;
while (pairs.size < grid + GENERATED_PAIRS) {
  const body = sequence(0, 5);
  const glob = `${chance(0.12) ? "!" : ""}${body.glob}${chance(0.06) ? "\\" : ""}`;
  const written = body.path();
  for (const path of [written, body.path(), mutated(written), mutated(body.path())]) {
    add(glob, path);
    if (glob.startsWith("**/")) add(glob, `${"z/".repeat(256)}${path}`);
  }
}

for (const [pattern, path] of HOSTILE) add(pattern, path);

const LONG_PATTERNS = [
  "**/*.ts",
  "src/**",
  "**/a/**",
  "{a,b}/**/*.{js,ts}",
  "@(a|b)/**",
  "*(a|b)",
  "**/{1..3}.ts",
  "!**/*.ts",
  "a/**/b",
  "[!a]*/**",
  "**/*é*.ts",
  "**/?é/*",
  `**/${"*[ab]".repeat(12)}`,
];

const LONG_PATHS = [
  "a/".repeat(300) + "b.ts",
  `src/${"ab".repeat(300)}.ts`,
  "a".repeat(600),
  `${"b/".repeat(256)}2.ts`,
  `${"aé/".repeat(200)}xéy.ts`,
  "ab".repeat(400),
];

for (const pattern of LONG_PATTERNS) for (const path of LONG_PATHS) add(pattern, path);

const SEQUENCES = [
  "{1....3}",
  "{1..3..}",
  "{-1..1}",
  "{a..C}",
  "{A..z}",
  "{001..3}",
  "{3..1}",
  "{1..10..0}",
];

const VALUES_IN_SEQUENCES = [
  "",
  "1",
  "2",
  "3",
  "-1",
  "0",
  "\\",
  "a",
  "C",
  "001",
  "002",
  "10",
  "b",
  "..",
];

for (const pattern of SEQUENCES) for (const path of VALUES_IN_SEQUENCES) add(pattern, path);

const globs = [...pairs.values()].map(
  ([pattern, path]) => [pattern, path, new Bun.Glob(pattern).match(path)] as const,
);

const WARM_AFTER = 40;
const byPattern = Map.groupBy(globs, ([pattern]) => pattern);
const cold = [...byPattern].flatMap(([pattern, rows]) => {
  const warm = new Bun.Glob(pattern);
  for (let use = 0; use < WARM_AFTER; use++) warm.match(rows[use % rows.length]![1]);
  return rows.filter(([, path, expected]) => warm.match(path) !== expected);
});

if (cold.length > 0)
  console.error(
    `width-tables: a warm Bun.Glob disagrees with a fresh one, which the table records, on ${JSON.stringify(cold)}`,
  );

const VALUES = [
  "",
  " ",
  "80",
  " 120 ",
  "\t100\n",
  " 100 ",
  "﻿90",
  "0",
  "-0",
  "+7",
  "-7",
  "007",
  "1.0",
  "1.5",
  ".5",
  "5.",
  ".",
  "1e2",
  "1e+2",
  "1E2",
  "1e-2",
  "1e",
  "e2",
  "1e400",
  "9007199254740993",
  "0x10",
  "0X1f",
  "0x",
  "-0x10",
  "0b101",
  "0o17",
  "0b2",
  "0o8",
  "0xg",
  "1_000",
  "Infinity",
  "-Infinity",
  "infinity",
  "inf",
  "nan",
  "NaN",
  "off",
  "tab",
  "1 2",
  "12abc",
  "١٢",
  "100;",
];

const numbers = VALUES.map((value) => [value, Number(value)]);
const json = (value: unknown) =>
  JSON.stringify(value, (_, item: unknown) =>
    typeof item === "number" && !Number.isFinite(item) ? String(item) : item,
  );

const files = [join(config, "bun-glob.json"), join(config, "number.json")];
await Bun.write(files[0]!, `${json(globs)}\n`);
await Bun.write(files[1]!, `${json(numbers)}\n`);

const formatted = Bun.spawnSync(
  [join(root, "node_modules", ".bin", "oxfmt"), "--disable-nested-config", ...files],
  { stdout: "inherit", stderr: "inherit" },
);

if (formatted.exitCode !== 0) process.exit(formatted.exitCode ?? 1);

console.log(`width-tables: ${globs.length} glob rows, ${numbers.length} number rows`);
