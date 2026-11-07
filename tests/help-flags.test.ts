import { expect, test } from "bun:test";
import { catalog, run } from "./support.ts";

test("CLI help prints aligned flag rows without tying --json to hooks", () => {
  const { flags } = catalog();
  const result = run("--help");
  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");

  const width = Math.max(...flags.map(([name]) => name.length));
  const output = result.stdout.split("\n");
  for (const [name, description] of flags)
    expect(output).toContain(`${name.padEnd(width)}  ${description}`);

  expect(flags.find(([name]) => name === "--json")![1]).toBe("findings as a JSON array");
});
