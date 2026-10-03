import { expect, test } from "bun:test";
import { existsSync, mkdirSync, renameSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { runHookCall } from "../src/cli.ts";
import { holds } from "../src/directories.ts";
import { run, scratch, scratchGitRepository } from "./support.ts";

const probe = scratch("case-probe");
writeFileSync(join(probe, "case"), "");
const caseInsensitive = existsSync(join(probe, "CASE"));

test.skipIf(!caseInsensitive)("directory selection preserves the on-disk file case", () => {
  const cwd = scratchGitRepository({
    files: { "src/case.ts": "function f(a: boolean) {\n  if (a) {\n    first();\n  }\n}\n" },
    staged: true,
  });

  renameSync(join(cwd, "src/case.ts"), join(cwd, "src/Case.ts"));

  const result = run({ cwd }, "--check", ".");
  expect(result.code).toBe(1);
  expect(result.stdout).toContain("src/Case.ts");
  expect(result.stdout).not.toContain("src/case.ts");
});

test("directory existence is refreshed for each invocation", () => {
  const cwd = scratch();
  writeFileSync(join(cwd, "package.json"), JSON.stringify({ devDependencies: { prettier: "*" } }));
  writeFileSync(join(cwd, "a.ts"), "export {};\n");

  const first = run({ cwd }, "--check", "a.ts");
  expect(first.code).toBe(0);
  expect(first.stderr).toBe("");

  mkdirSync(join(cwd, ".editorconfig"));
  writeFileSync(join(cwd, "b.ts"), "export {};\n");

  const second = run({ cwd }, "--check", "b.ts");
  expect(second.code).toBe(0);
  expect(second.stderr).toContain(
    "stanza: could not read the line width from .editorconfig, so stanza used the next source for it",
  );
});

test("a hook call outside main starts its own invocation", () => {
  const cwd = scratch();
  const io = {
    cwd,
    env: {},
    stdin: () => new Uint8Array(),
    stdout: () => {},
    stderr: () => {},
  };

  expect(holds(cwd, "late.json")).toBe(false);

  writeFileSync(join(cwd, "late.json"), "{}\n");
  expect(holds(cwd, "late.json")).toBe(false);

  expect(runHookCall({ event: "edit", cwd, paths: [] }, [], io)).toBe(0);
  expect(holds(cwd, "late.json")).toBe(true);
});
