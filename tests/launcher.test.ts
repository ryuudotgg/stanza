import { expect, test } from "bun:test";
import { dirname, join } from "node:path";

const root = join(import.meta.dir, "..");
const smoke = join(root, "scripts", "smoke.sh");
const launcher = join(root, "src", "launcher");

test.skipIf(!Bun.which("node"))("launcher reports how to install Bun when it is missing", () => {
  const result = Bun.spawnSync([smoke, "--without-bun", launcher]);
  expect(result.exitCode, new TextDecoder().decode(result.stderr)).toBe(0);
});

test("launcher runs with Bun and without Node", () => {
  const path = `${dirname(process.execPath)}:/usr/bin:/bin`;
  const result = Bun.spawnSync([smoke, launcher], { env: { ...process.env, PATH: path } });
  expect(result.exitCode, new TextDecoder().decode(result.stderr)).toBe(0);
});
