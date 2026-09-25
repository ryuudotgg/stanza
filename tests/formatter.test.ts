import { describe, expect, test } from "bun:test";
import { cpSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const repo = join(import.meta.dir, "..");
const cli = join(repo, "src", "cli.ts");
const oxfmt = join(repo, "node_modules", ".bin", "oxfmt");
const fixtures = join(import.meta.dir, "fixtures");

interface Tool {
  name: string;
  run(dir: string, files: string[]): void;
}

interface Move {
  tool: string;
  file: string;
}

function spawn(cmd: string[], cwd: string, maxCode: number): string {
  const result = Bun.spawnSync(cmd, { cwd });
  const decoder = new TextDecoder();
  if (result.exitCode > maxCode)
    throw new Error(`${cmd.join(" ")} exited ${result.exitCode}: ${decoder.decode(result.stderr)}`);

  return decoder.decode(result.stdout);
}

const formatter: Tool = {
  name: "oxfmt",
  run(dir, files) {
    spawn([oxfmt, ...files], dir, 0);
  },
};

const stanza: Tool = {
  name: "stanza --fix",
  run(dir, files) {
    spawn(["bun", "run", cli, "--fix", ...files], dir, 1);
  },
};

function snapshot(dir: string, files: string[]): string[] {
  return files.map((file) => readFileSync(join(dir, file), "utf8"));
}

function fixedPoint(dir: string, files: string[], round: Tool[]): Move[] {
  for (const tool of round) tool.run(dir, files);

  const settled = snapshot(dir, files);
  const moves: Move[] = [];
  for (const tool of round) {
    tool.run(dir, files);
    const texts = snapshot(dir, files);
    for (const [index, file] of files.entries())
      if (texts[index] !== settled[index]) moves.push({ tool: tool.name, file });
  }

  return moves;
}

function findings(dir: string, files: string[]): Map<string, number> {
  const counts = new Map<string, number>();
  const output = spawn(["bun", "run", cli, "--check", ...files], dir, 1);
  for (const line of output.split("\n")) {
    const match = /^(.+):\d+:\d+ (\S+) /.exec(line);
    if (!match) continue;

    const key = `${match[1]} ${match[2]}`;
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }

  return counts;
}

const settleInTwoRounds = new Set(["braces/asi.after.ts", "braces/dangling-with.after.js"]);

for (const name of readdirSync(fixtures).sort()) {
  const source = join(fixtures, name);

  describe(`${name}: stanza and oxfmt settle in one round`, () => {
    for (const file of readdirSync(source).sort()) {
      if (!/\.after\.[jt]sx?$/.test(file)) continue;

      test.failingIf(settleInTwoRounds.has(`${name}/${file}`))(file, () => {
        const dir = mkdtempSync(join(tmpdir(), "stanza-oxfmt-"));
        cpSync(source, dir, { recursive: true });

        expect(fixedPoint(dir, [file], [formatter, stanza])).toEqual([]);

        const inPlace = findings(source, [file]);
        const introduced = [...findings(dir, [file])]
          .filter(([key, count]) => count > (inPlace.get(key) ?? 0))
          .map(([key]) => key);

        expect(introduced).toEqual([]);
      });
    }
  });
}

test("transforms that undo each other have no fixed point", () => {
  const dir = mkdtempSync(join(tmpdir(), "stanza-oxfmt-"));
  writeFileSync(join(dir, "a.ts"), "export const a = 1;\n");

  const marker = "// added\n";
  const add: Tool = {
    name: "add",
    run(at, files) {
      for (const file of files)
        writeFileSync(join(at, file), marker + readFileSync(join(at, file), "utf8"));
    },
  };

  const strip: Tool = {
    name: "strip",
    run(at, files) {
      for (const file of files)
        writeFileSync(join(at, file), readFileSync(join(at, file), "utf8").replace(marker, ""));
    },
  };

  expect(fixedPoint(dir, ["a.ts"], [add, strip])).toEqual([{ tool: "add", file: "a.ts" }]);
});
