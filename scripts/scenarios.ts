import { rmSync } from "node:fs";
import { join, resolve } from "node:path";

const root = join(import.meta.dir, "..");
const [test, out] = process.argv.slice(2);
if (!test || !out) {
  console.error("Usage: bun scripts/scenarios.ts <test file> <out dir>");
  process.exit(2);
}

const target = resolve(out);
const scenarios = join(root, "tests", "scenarios");
if (!target.startsWith(`${scenarios}/`)) {
  console.error(`scenarios: the out dir must be under ${scenarios}`);
  process.exit(2);
}

rmSync(target, { recursive: true, force: true });

const result = Bun.spawnSync(["bun", "test", resolve(test)], {
  cwd: root,
  env: { ...process.env, STANZA_KEEP_SCRATCH: target },
  stdout: "inherit",
  stderr: "inherit",
});

process.exit(result.exitCode ?? 1);
