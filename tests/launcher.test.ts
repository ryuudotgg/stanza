import { expect, test } from "bun:test";
import { join } from "node:path";
import { cpuOf } from "./budget.ts";

const root = join(import.meta.dir, "..");
const smoke = join(root, "scripts", "smoke.sh");
const launcher = join(root, "src", "launcher");

test.skipIf(!Bun.which("node"))(
  "launcher reports how to install Bun when it is missing",
  () => {
    const result = Bun.spawnSync([smoke, "--without-bun", launcher]);
    expect(result.exitCode, new TextDecoder().decode(result.stderr)).toBe(0);
    expect(cpuOf(result)).toBeLessThan(5_000);
  },
  60_000,
);

test("launcher runs with Bun and without Node", () => {
  const result = Bun.spawnSync([smoke, "--without-node", launcher]);
  expect(result.exitCode, new TextDecoder().decode(result.stderr)).toBe(0);
  expect(cpuOf(result)).toBeLessThan(5_000);
}, 60_000);
