import { expect, test } from "bun:test";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { run, scratch } from "./support.ts";

function splitGuards(count: number): string {
  return Array.from(
    { length: count },
    (_, index) => `${index === 3 ? "\n" : ""}  if (status === ${401 + index}) return "${index}";`,
  ).join("\n");
}

function body(statements: string): string {
  return `function run(status, rows) {\n${statements}\n}\n`;
}

const lets = body(
  "  let alpha = 0;\n  let bravo = 0;\n  let charlie = 0;\n\n  let delta = 0;\n  let echo = 0;\n  let foxtrot = 0;\n  for (const row of rows) alpha += bravo + charlie + delta + echo + foxtrot + row;",
);

for (const [name, source] of [
  ["six guards followed by a splittable final return", body(`${splitGuards(6)}\n  return "z";`)],
  ["six lets read by one following loop", lets],
] as const)
  test(`${name}: CLI fix, check and second fix agree`, () => {
    const directory = scratch("wall-forced-joins");
    const path = join(directory, "wall.ts");
    writeFileSync(path, source);

    const first = run({ cwd: directory }, "--fix", "--no-braces", path);
    expect(first.code).toBe(0);
    expect(first.stdout).toBe("");
    expect(first.stderr).toBe("rewrote 1 file, no findings left\n");

    const fixed = readFileSync(path, "utf8");
    expect(fixed).not.toBe(source);
    expect(fixed).not.toContain("\n\n");

    for (const mode of ["--check", "--fix"]) {
      const result = run({ cwd: directory }, mode, "--no-braces", path);
      expect(result.code).toBe(0);
      expect(result.stdout).toBe("");
      expect(result.stderr).toBe(
        mode === "--check" ? "checked 1 file, no findings\n" : "checked 1 file, nothing to fix\n",
      );

      expect(readFileSync(path, "utf8")).toBe(fixed);
    }
  });
