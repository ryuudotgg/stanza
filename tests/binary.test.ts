import { expect, test } from "bun:test";
import { cpSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
const fixtures = join(root, "tests", "fixtures");
const cli = join(root, "src", "cli.ts");

const appleSilicon = process.platform === "darwin" && process.arch === "arm64";

function run(
  command: string[],
  cwd = root,
): { code: number | null; stdout: string; stderr: string } {
  const result = Bun.spawnSync(command, { cwd });
  return {
    code: result.exitCode,
    stdout: new TextDecoder().decode(result.stdout),
    stderr: new TextDecoder().decode(result.stderr),
  };
}

function snapshot(dir: string): Record<string, string> {
  const files = readdirSync(dir, { recursive: true, withFileTypes: true }).filter((entry) =>
    entry.isFile(),
  );

  return Object.fromEntries(
    files.map((entry) => {
      const path = join(entry.parentPath, entry.name);
      return [path.slice(dir.length), readFileSync(path, "utf8")];
    }),
  );
}

test.skipIf(!appleSilicon)(
  "the compiled binary checks and fixes the fixtures exactly like a source run",
  () => {
    const scratch = mkdtempSync(join(tmpdir(), "stanza-binary-"));
    try {
      const binary = join(scratch, "stanza");
      const build = run([
        process.execPath,
        "build",
        "--compile",
        "--minify",
        "src/compile.ts",
        "--outfile",
        binary,
      ]);

      expect(build.stderr).toBe("");
      expect(build.code).toBe(0);

      expect(run(["codesign", "-s", "-", "-f", binary]).code).toBe(0);

      const compiledCheck = run([binary, "--check", "tests/fixtures"]);
      const sourceCheck = run([process.execPath, "run", cli, "--check", "tests/fixtures"]);
      expect(sourceCheck.code).toBe(1);

      expect(compiledCheck.stderr).toBe("");
      expect(compiledCheck.stdout).toBe(sourceCheck.stdout);
      expect(compiledCheck.code).toBe(sourceCheck.code);

      const compiledTree = join(scratch, "compiled");
      const sourceTree = join(scratch, "source");
      cpSync(fixtures, compiledTree, { recursive: true });
      cpSync(fixtures, sourceTree, { recursive: true });

      const compiledFix = run([binary, "--fix", "."], compiledTree);
      const sourceFix = run([process.execPath, "run", cli, "--fix", "."], sourceTree);
      expect(compiledFix.stderr).toBe("");
      expect(compiledFix.stdout).toBe(sourceFix.stdout);
      expect(compiledFix.code).toBe(sourceFix.code);

      expect(snapshot(sourceTree)).not.toEqual(snapshot(fixtures));
      expect(snapshot(compiledTree)).toEqual(snapshot(sourceTree));
    } finally {
      rmSync(scratch, { recursive: true, force: true });
    }
  },
  { timeout: 60_000 },
);
