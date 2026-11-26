import { expect, test } from "bun:test";
import { existsSync, renameSync, writeFileSync } from "node:fs";
import { join } from "node:path";
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

test.skipIf(!caseInsensitive)("selection preserves the on-disk directory case", () => {
  const cwd = scratchGitRepository({
    files: {
      "src/b.ts": "export {};\n",
      "src/lib/a.ts": "function f(a: boolean) {\n  if (a) {\n    first();\n  }\n}\n",
    },
    staged: true,
  });

  renameSync(join(cwd, "src/lib"), join(cwd, "src/Lib"));

  for (const path of [".", "src/lib/a.ts", "src/LIB/A.ts"]) {
    const result = run({ cwd }, "--check", path);
    expect(result.code).toBe(1);
    expect(result.stdout).toContain("src/Lib/a.ts");
    expect(result.stdout).not.toContain("src/lib/");
  }
});
