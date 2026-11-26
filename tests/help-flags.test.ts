import { expect, test } from "bun:test";
import catalog from "../docs/lib/catalog.json";
import { run } from "./support.ts";

test("--help and -h fit in 80 columns", () => {
  for (const flag of ["--help", "-h"]) {
    const lines = run(flag).stdout.split("\n");
    expect(lines.filter((line) => Bun.stringWidth(line) > 80)).toEqual([]);
  }
});

test("CLI help prints aligned flag rows without tying --json to hooks", () => {
  const flags = catalog.flags as [string, string][];
  const result = run("--help");
  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");

  const width = Math.max(...flags.map(([name]) => name.length));
  const continuation = " ".repeat(width + 4);
  const rows: string[] = [];
  for (const line of result.stdout.split("\n"))
    if (line.startsWith(continuation) && rows.length > 0)
      rows[rows.length - 1] += ` ${line.trim()}`;
    else rows.push(line);

  for (const [name, description] of flags)
    expect(rows).toContain(`  ${name.padEnd(width)}  ${description}`);

  expect(flags.find(([name]) => name === "--json")![1]).toBe("findings as a JSON array");
});

test("--help prints every hook registration and the rule catalog", () => {
  const { registrations, rules } = catalog;
  const result = run("--help");
  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");

  const lines = result.stdout.split("\n");
  for (const [client, { files, settings }] of Object.entries(registrations)) {
    const heading = lines.indexOf(`${client} ${files.join(" or ")}:`);
    expect(heading, client).toBeGreaterThanOrEqual(0);

    const end = lines.indexOf("", heading + 1);
    expect(JSON.parse(lines.slice(heading + 1, end).join("\n"))).toEqual(settings);
  }

  for (const rule of Object.keys(rules)) expect(result.stdout).toContain(rule);
});
