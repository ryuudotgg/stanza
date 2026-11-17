import { join } from "node:path";

const root = join(import.meta.dir, "..");
const config = join(root, "crates", "stanza", "src", "languages", "javascript", "config");

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

const file = join(config, "number.json");
await Bun.write(file, `${json(numbers)}\n`);

const formatted = Bun.spawnSync(
  [join(root, "node_modules", ".bin", "oxfmt"), "--disable-nested-config", file],
  { stdout: "inherit", stderr: "inherit" },
);

if (formatted.exitCode !== 0) process.exit(formatted.exitCode ?? 1);

console.log(`width-tables: ${numbers.length} number rows`);
