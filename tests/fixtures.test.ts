import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { processFile } from "../src/index.ts";
import { bracesEnforced } from "../src/config.ts";

const root = join(import.meta.dir, "fixtures");

function normalize(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.replace(/[{}]/g, "").replace(/\s+/g, " ").trim())
    .filter((line) => line.length > 0);
}

for (const dir of readdirSync(root).sort()) {
  const dirPath = join(root, dir);
  const enforcedBraces = bracesEnforced(dirPath);

  describe(dir, () => {
    for (const file of readdirSync(dirPath).sort()) {
      const match = /^(.+)\.before\.(tsx?|jsx?)$/.exec(file);
      if (!match) continue;

      const [, name, ext] = match;
      const beforePath = join(dirPath, file);
      const afterPath = join(dirPath, `${name}.after.${ext}`);
      const findingsPath = join(dirPath, `${name}.findings`);

      const before = readFileSync(beforePath, "utf8");
      const after = readFileSync(afterPath, "utf8");
      const expectedFindings = existsSync(findingsPath)
        ? readFileSync(findingsPath, "utf8").trim().split("\n").filter(Boolean)
        : [];

      test(`${name}: fix matches after`, () => {
        const result = processFile(beforePath, before, "fix", { enforcedBraces });
        expect(result.parseError).toBe(false);
        expect(result.text).toBe(after);
      });

      test(`${name}: fix is idempotent`, () => {
        const once = processFile(beforePath, before, "fix", { enforcedBraces });
        const twice = processFile(beforePath, once.text, "fix", { enforcedBraces });
        expect(twice.text).toBe(once.text);
      });

      test(`${name}: only blank lines and braces change`, () => {
        expect(normalize(after)).toEqual(normalize(before));
      });

      test(`${name}: check on after reports only report-only findings`, () => {
        const result = processFile(afterPath, after, "check", { enforcedBraces });
        const fixable = result.findings.filter((f) => f.fixable);
        expect(fixable).toEqual([]);

        const reported = result.findings.map((f) => `${f.line}:${f.col} ${f.rule}`);
        expect(reported).toEqual(expectedFindings);
      });

      if (before !== after)
        test(`${name}: check on before reports fixable findings`, () => {
          const result = processFile(beforePath, before, "check", { enforcedBraces });
          expect(result.text).toBe(before);
          expect(result.findings.some((f) => f.fixable)).toBe(true);
        });
    }
  });
}
