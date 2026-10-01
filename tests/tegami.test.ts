import { expect, test } from "bun:test";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { scratch } from "./support.ts";

const script = join(import.meta.dirname, "..", "scripts", "tegami.ts");

function version(withNote: boolean): { cwd: string; code: number; output: string } {
  const cwd = scratch("tegami");
  const env = { ...process.env };
  for (const key of ["CI", "GITHUB_TOKEN", "GH_TOKEN", "GITHUB_REPOSITORY"]) delete env[key];

  writeFileSync(
    join(cwd, "package.json"),
    JSON.stringify({
      name: "@ryuugg/stanza",
      version: "0.1.0",
      repository: { url: "git+https://github.com/ryuudotgg/stanza.git" },
    }),
  );

  mkdirSync(join(cwd, ".tegami"));

  if (withNote)
    writeFileSync(
      join(cwd, ".tegami", "note.md"),
      '---\npackages:\n  "@ryuugg/stanza": patch\n---\n\n### Title\n\nOne line of text.\n',
    );

  for (const args of [
    ["git", "init", "-q"],
    ["git", "add", "."],
    [
      "git",
      "-c",
      "user.name=t",
      "-c",
      "user.email=t@t",
      "-c",
      "commit.gpgsign=false",
      "commit",
      "-m",
      "test: seed scratch repo",
    ],
  ]) {
    const result = Bun.spawnSync(args, { cwd, env });
    expect(result.exitCode, new TextDecoder().decode(result.stderr)).toBe(0);
  }

  const result = Bun.spawnSync([process.execPath, script, "version"], { cwd, env });
  const decoder = new TextDecoder();
  return {
    cwd,
    code: result.exitCode,
    output: decoder.decode(result.stdout) + decoder.decode(result.stderr),
  };
}

test("version consumes a patch note and writes the changelog and publish lock", () => {
  const result = version(true);

  expect(result.code, result.output).toBe(0);
  expect(JSON.parse(readFileSync(join(result.cwd, "package.json"), "utf8")).version).toBe("0.1.1");

  const changelog = readFileSync(join(result.cwd, "CHANGELOG.md"), "utf8");
  expect(changelog.startsWith("## 0.1.1")).toBe(true);
  expect(changelog).toContain("### Title");
  expect(existsSync(join(result.cwd, ".tegami", "publish-lock.yaml"))).toBe(true);
  expect(existsSync(join(result.cwd, ".tegami", "note.md"))).toBe(false);
});

test("version rejects a repo with no pending notes", () => {
  const result = version(false);
  expect(result.code).not.toBe(0);
  expect(result.output).toContain("no pending notes");
});
