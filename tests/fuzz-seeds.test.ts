import { describe, expect, test } from "bun:test";
import { cpSync, existsSync, mkdirSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { scratch } from "./support.ts";

const repository = join(import.meta.dir, "..");
function project(): string {
  const root = scratch("fuzz-seeds");
  cpSync(join(repository, "scripts", "fuzz-seeds.ts"), join(root, "scripts", "fuzz-seeds.ts"));
  cpSync(join(repository, "fuzz", "seeds"), join(root, "fuzz", "seeds"), { recursive: true });
  return root;
}

function input(files: Record<string, string>): string {
  const directory = scratch("fuzz-input");
  for (const [name, text] of Object.entries(files)) writeFileSync(join(directory, name), text);
  return directory;
}

function seed(root: string, ...args: string[]): { code: number; stdout: string; stderr: string } {
  const result = Bun.spawnSync(
    [process.execPath, join(root, "scripts", "fuzz-seeds.ts"), ...args],
    {
      cwd: root,
    },
  );

  return {
    code: result.exitCode,
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
  };
}

function corpus(root: string, target = ""): string[] {
  const directory = join(root, "fuzz", "corpus", target);
  return existsSync(directory) ? readdirSync(directory) : [];
}

function reported(stdout: string): Record<string, number> {
  return Object.fromEntries(
    [...stdout.matchAll(/^(\w+): (\d+) new seeds?$/gm)].map((match) => [
      match[1]!,
      Number(match[2]),
    ]),
  );
}

describe("fuzz-seeds", () => {
  test("counts only the seeds it writes, and nothing on a second run", () => {
    const root = project();
    const files = input({ "a.ts": "const a = 1;\n", "b.ts": "const a = 1;\n", "a.json": "{}\n" });

    const first = seed(root, files, files);

    expect(first.code).toBe(0);
    const counts = reported(first.stdout);
    for (const target of ["format", "hook_input", "git_diff", "globs"])
      expect(counts[target]).toBe(corpus(root, target).length);

    expect(counts.format).toBe(1);
    expect(counts.globs).toBeGreaterThan(0);

    const second = seed(root, files, files);

    expect(reported(second.stdout)).toEqual({ format: 0, hook_input: 0, git_diff: 0, globs: 0 });
  });

  test("--target seeds one target into its own folder", () => {
    const root = project();

    const result = seed(root, "--target", "globs", input({}));

    expect(result.code).toBe(0);
    expect(Object.keys(reported(result.stdout))).toEqual(["globs"]);
    expect(corpus(root)).toEqual(["globs"]);
    expect(corpus(root, "globs").length).toBeGreaterThan(0);
  });

  test("an unknown target exits 2 with usage and writes nothing", () => {
    const root = project();

    const result = seed(root, "--target", "nope", input({}));

    expect(result.code).toBe(2);
    expect(result.stderr).toContain("usage: bun scripts/fuzz-seeds.ts");
    expect(existsSync(join(root, "fuzz", "corpus"))).toBe(false);
  });

  test("skips a source over the seed cap, selector byte included", () => {
    const root = project();
    const files = input({ "edge.js": "a".repeat(64 * 1024), "fits.js": "a".repeat(64 * 1024 - 1) });

    const result = seed(root, "--target", "format", files);

    expect(reported(result.stdout)).toEqual({ format: 1 });
  });

  test("reads the maintained seeds when an input holds them under a skipped folder", () => {
    const root = project();
    const outer = scratch("fuzz-outer");
    mkdirSync(join(outer, "target"));
    cpSync(root, join(outer, "target", "checkout"), { recursive: true });

    const result = seed(join(outer, "target", "checkout"), "--target", "globs", outer);

    expect(reported(result.stdout).globs).toBeGreaterThan(0);
  });
});
