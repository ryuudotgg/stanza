import { expect, test } from "bun:test";
import { cpSync, existsSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { hostPlatform } from "../scripts/platform.ts";
import { THRESHOLD } from "../src/pool.ts";
import { gitBinary, scratch } from "./support.ts";

const root = join(import.meta.dir, "..");
const fixtures = join(root, "tests", "fixtures");
const cli = join(root, "src", "cli.ts");

const entry = join(root, "src", "compile", `${hostPlatform()}.ts`);

function run(
  command: string[],
  cwd = root,
  env = process.env,
): { code: number | null; stdout: string; stderr: string } {
  const result = Bun.spawnSync(command, { cwd, env });
  return {
    code: result.exitCode,
    stdout: new TextDecoder().decode(result.stdout),
    stderr: new TextDecoder().decode(result.stderr),
  };
}

function snapshot(dir: string): Record<string, { bytes: string; mode: number }> {
  const files = readdirSync(dir, { recursive: true, withFileTypes: true }).filter((entry) =>
    entry.isFile(),
  );

  return Object.fromEntries(
    files.map((entry) => {
      const path = join(entry.parentPath, entry.name);
      return [
        path.slice(dir.length),
        { bytes: readFileSync(path).toString("base64"), mode: statSync(path).mode },
      ];
    }),
  );
}

for (const bytecode of [false, true])
  test.skipIf(!existsSync(entry))(
    `the pooled ${bytecode ? "bytecode" : "plain"} binary loads raw transfer and matches serial source check and fix`,
    () => {
      const directory = scratch("binary");
      const binary = join(directory, "stanza");
      const workerLog = join(directory, "worker-chunks");
      const preload = join(directory, "observe-build.ts");
      const observation = `if ("outcomes" in message && message.outcomes.some((outcome) => outcome !== null)) appendFileSync(${JSON.stringify(workerLog)}, "chunk\\n");\n  data.port.postMessage(message);`;
      writeFileSync(
        preload,
        `import { readFileSync } from "node:fs";
const original = Bun.build;
Bun.build = (options) => original({
  ...options,
  bytecode: ${bytecode},
  plugins: [{
    name: "observe-worker-chunks",
    setup(build) {
      if (${bytecode}) build.onLoad({ filter: /\\/src\\/compile\\/[^/]+\\.ts$/ }, ({ path }) => ({
        loader: "ts",
        contents: readFileSync(path, "utf8").replace("await start(addon);", "void start(addon);"),
      }));
      build.onLoad({ filter: /\\/src\\/pool\\.ts$/ }, ({ path }) => ({
        loader: "ts",
        contents: 'import { appendFileSync } from "node:fs";\\n' + readFileSync(path, "utf8").replace("data.port.postMessage(message);", ${JSON.stringify(observation)}),
      }));
    },
  }],
});\n`,
      );

      const build = run([
        process.execPath,
        "--preload",
        preload,
        "scripts/build.ts",
        "--outdir",
        directory,
      ]);

      expect(build.stderr).toBe("");
      expect(build.code).toBe(0);

      const revision = run([gitBinary, "rev-parse", "--short", "HEAD"]);
      const commit = revision.stdout.trim();
      expect(revision.code).toBe(0);
      expect(commit).not.toBe("");

      const version = run([binary, "--version"]);
      expect(version.code).toBe(0);
      expect(version.stdout).toContain(commit);

      const checkedTree = join(directory, "checked");
      const candidates = Object.keys(snapshot(fixtures)).filter((path) =>
        /\.[cm]?[jt]sx?$/.test(path),
      );

      const copies = Math.ceil((THRESHOLD * 4) / candidates.length);
      for (let copy = 0; copy < copies; copy++)
        cpSync(fixtures, join(checkedTree, String(copy)), { recursive: true });

      expect(
        Object.keys(snapshot(checkedTree)).filter((path) => /\.[cm]?[jt]sx?$/.test(path)).length,
      ).toBeGreaterThanOrEqual(THRESHOLD * 2);

      const pooledEnv = { ...process.env, STANZA_WORKERS: "2", STANZA_RAW_TRANSFER: "1" };
      const serialEnv = { ...process.env, STANZA_WORKERS: "0", STANZA_RAW_TRANSFER: "1" };

      const compiledCheck = run([binary, "--check", "."], checkedTree, pooledEnv);
      const sourceCheck = run(
        [process.execPath, "run", cli, "--check", "."],
        checkedTree,
        serialEnv,
      );

      expect(sourceCheck.code).toBe(1);

      expect(compiledCheck.stderr).toBe("");
      expect(compiledCheck.stdout).toBe(sourceCheck.stdout);
      expect(compiledCheck.code).toBe(sourceCheck.code);
      expect(readFileSync(workerLog, "utf8")).toContain("chunk\n");

      writeFileSync(workerLog, "");

      const compiledTree = join(directory, "compiled");
      const sourceTree = join(directory, "source");
      cpSync(checkedTree, compiledTree, { recursive: true });
      cpSync(checkedTree, sourceTree, { recursive: true });

      const compiledFix = run([binary, "--fix", "."], compiledTree, pooledEnv);
      const sourceFix = run([process.execPath, "run", cli, "--fix", "."], sourceTree, serialEnv);
      expect(compiledFix.stderr).toBe("");
      expect(compiledFix.stdout).toBe(sourceFix.stdout);
      expect(compiledFix.code).toBe(sourceFix.code);

      expect(readFileSync(workerLog, "utf8")).toContain("chunk\n");

      expect(snapshot(sourceTree)).not.toEqual(snapshot(checkedTree));
      expect(snapshot(compiledTree)).toEqual(snapshot(sourceTree));
    },
    { timeout: 60_000 },
  );

test("the build script rejects an unknown platform before installing or building", () => {
  const directory = scratch("build");
  const build = run([
    process.execPath,
    "scripts/build.ts",
    "--outdir",
    directory,
    "--platform",
    "solaris-sparc",
  ]);

  expect(build.code).toBe(1);
  expect(build.stderr).toContain("Unknown platform solaris-sparc");
  expect(build.stderr).toContain("darwin-arm64");
  expect(readdirSync(directory)).toEqual([]);
});
