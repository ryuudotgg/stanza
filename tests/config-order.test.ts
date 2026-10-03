import { expect, test } from "bun:test";
import { mkdirSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { braceDecisions } from "../src/languages/javascript/config/index.ts";
import { scratch } from "./support.ts";

function chain(prefix: string, levels: number): string {
  const lines = [`const ${prefix}0 = { rules: { curly: "off" } };`];
  for (let level = 1; level <= levels; level++)
    lines.push(
      `const ${prefix}${level} = [{ files: ["**/*.ts"], extends: [${prefix}${level - 1}] }, { files: ["**/*.js"], extends: [${prefix}${level - 1}] }];`,
    );

  return lines.join("\n");
}

function fixture(shared = 12, own = 14, twice = false): string {
  const dir = scratch("config");
  const extra = twice ? '{ files: ["**/*.ts"], extends: ["../shared.js"] }, ' : "";
  const files: Record<string, string> = {
    "shared.js": `${chain("s", shared)}\nexport default [s${shared}, { rules: { curly: "off" } }];\n`,
    "a/eslint.config.js": 'export default [{ extends: ["../shared.js"] }];\n',
    "b/eslint.config.js": `${chain("o", own)}\nexport default [{ extends: ["../shared.js"] }, ${extra}o${own}, { rules: { curly: "off" } }];\n`,
  };

  for (const [name, text] of Object.entries(files)) {
    mkdirSync(dirname(join(dir, name)), { recursive: true });
    writeFileSync(join(dir, name), text);
  }

  return dir;
}

test("a config's budget answer does not depend on which directory warmed its shared extends", () => {
  const cold = fixture();
  const coldSetting = braceDecisions(join(cold, "b"), ".ts")[0]?.setting;

  const warm = fixture();
  expect(braceDecisions(join(warm, "a"), ".ts")).toEqual([
    {
      family: "flat",
      setting: "off",
      files: [join(realpathSync(warm), "a", "eslint.config.js")],
    },
  ]);

  const warmSetting = braceDecisions(join(warm, "b"), ".ts")[0]?.setting;
  expect(warmSetting).toBe(coldSetting);
  expect(coldSetting).toBe("unknown");
});

test("a config that extends one file twice counts that file once", () => {
  const cold = fixture(13, 10, true);
  expect(braceDecisions(join(cold, "b"), ".ts")[0]?.setting).toBe("off");

  const warm = fixture(13, 10, true);
  expect(braceDecisions(join(warm, "a"), ".ts")[0]?.setting).toBe("off");

  expect(braceDecisions(join(warm, "b"), ".ts")[0]?.setting).toBe("off");
});
