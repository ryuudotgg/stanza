import { describe, expect, test } from "bun:test";
import { cpSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { decode, formatText, type StepResult } from "../src/step.ts";
import type { Mode } from "../src/types.ts";
import { scratch } from "./support.ts";

const repo = join(import.meta.dir, "..");
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

function spawn(cmd: string[], cwd: string): void {
  const result = Bun.spawnSync(cmd, { cwd });
  if (result.exitCode !== 0)
    throw new Error(
      `${cmd.join(" ")} exited ${result.exitCode}: ${new TextDecoder().decode(result.stderr)}`,
    );
}

const formatter: Tool = {
  name: "oxfmt",
  run(dir, files) {
    spawn([oxfmt, ...files], dir);
  },
};

function stanzaFile(dir: string, file: string, mode: Mode): StepResult {
  const path = join(dir, file);
  const result = formatText(path, decode(readFileSync(path)), { mode });
  if (result.parseError) throw new Error(`stanza failed to parse ${file}`);
  return result;
}

const stanza: Tool = {
  name: "stanza --fix",
  run(dir, files) {
    for (const file of files) {
      const { fixed } = stanzaFile(dir, file, "fix");
      if (fixed !== undefined) writeFileSync(join(dir, file), fixed, "utf8");
    }
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

function findings(dir: string, file: string): string[] {
  return stanzaFile(dir, file, "check").findings.map(
    (finding) => `${finding.line}:${finding.col} ${finding.rule}`,
  );
}

const settleInTwoRounds = new Set([
  "braces/asi.after.ts",
  "braces/dangling-with.after.js",
  "braces/empty-statement.after.ts",
]);

for (const name of readdirSync(fixtures).sort()) {
  const source = join(fixtures, name);

  describe(`${name}: stanza and oxfmt settle in one round`, () => {
    for (const file of readdirSync(source).sort()) {
      if (!/\.after\.[jt]sx?$/.test(file)) continue;

      test(file, () => {
        const dir = scratch("oxfmt");
        cpSync(source, dir, { recursive: true });

        const round = [formatter, stanza];
        const twoRounds = settleInTwoRounds.has(`${name}/${file}`);
        const firstMoves = twoRounds ? round.map((tool) => ({ tool: tool.name, file })) : [];
        expect(fixedPoint(dir, [file], round)).toEqual(firstMoves);

        if (twoRounds) expect(fixedPoint(dir, [file], round)).toEqual([]);

        const inPlace = findings(source, file);
        expect(findings(dir, file).filter((finding) => !inPlace.includes(finding))).toEqual([]);
      });
    }
  });
}

test("transforms that undo each other have no fixed point", () => {
  const dir = scratch("oxfmt");
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
