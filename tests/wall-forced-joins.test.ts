import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { formatText } from "../src/step.ts";

function guards(count: number, split = false): string {
  return Array.from(
    { length: count },
    (_, index) =>
      `${split && index === 3 ? "\n" : ""}  if (status === ${401 + index}) return "${index}";`,
  ).join("\n");
}

function calls(count = 6): string {
  return Array.from({ length: count }, (_, index) => `  console.log(${index});`).join("\n");
}

function body(statements: string): string {
  return `function run(status, rows) {\n${statements}\n}\n`;
}

function walls(source: string, mode: "check" | "fix" = "check") {
  const result = formatText("wall.ts", source, { mode, braces: "off" });
  expect(result.parseError).toBe(false);
  return result.findings.filter((finding) => finding.rule === "wall");
}

const lets = body(
  "  let alpha = 0;\n  let bravo = 0;\n  let charlie = 0;\n\n  let delta = 0;\n  let echo = 0;\n  let foxtrot = 0;\n  for (const row of rows) alpha += bravo + charlie + delta + echo + foxtrot + row;",
);

for (const [name, source] of [
  ["six guards followed by a splittable final return", body(`${guards(6, true)}\n  return "z";`)],
  ["six lets read by one following loop", lets],
] as const)
  test(`${name}: CLI fix, check and second fix agree`, () => {
    const directory = mkdtempSync(join(tmpdir(), "stanza-wall-forced-joins-"));
    const path = join(directory, "wall.ts");
    const cli = join(import.meta.dir, "../src/cli.ts");
    writeFileSync(path, source);

    try {
      const first = Bun.spawnSync([process.execPath, cli, "--fix", "--no-braces", path], {
        cwd: directory,
      });

      expect(first.exitCode).toBe(0);
      expect(first.stdout.toString()).toBe("");
      expect(first.stderr.toString()).toBe("");

      const fixed = readFileSync(path, "utf8");
      expect(fixed).not.toBe(source);
      expect(fixed).not.toContain("\n\n");

      for (const mode of ["--check", "--fix"]) {
        const result = Bun.spawnSync([process.execPath, cli, mode, "--no-braces", path], {
          cwd: directory,
        });

        expect(result.exitCode).toBe(0);
        expect(result.stdout.toString()).toBe("");
        expect(result.stderr.toString()).toBe("");
        expect(readFileSync(path, "utf8")).toBe(fixed);
      }
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });

test("ordinary six-call walls remain actionable", () => {
  const source = body(calls());
  expect(walls(source)).toEqual([expect.objectContaining({ line: 2, fixable: false })]);
  expect(walls(source, "fix")).toHaveLength(1);

  const split = source.replace("  console.log(3);", "\n  console.log(3);");
  expect(walls(split)).toEqual([]);
  expect(formatText("wall.ts", split, { mode: "fix", braces: "off" }).fixed).toBeUndefined();
});

test("five joined guards and a return still form a clearable wall", () => {
  const source = body(`${guards(5)}\n  return "z";`);
  expect(walls(source)).toHaveLength(1);
  expect(walls(source, "fix")).toHaveLength(1);

  const split = source.replace('  return "z";', '\n  return "z";');
  expect(walls(split)).toEqual([]);
  expect(formatText("wall.ts", split, { mode: "fix", braces: "off" }).fixed).toBeUndefined();
});

test("shorter use-joined groups still count toward mixed walls", () => {
  const source = body(
    "  let alpha = 0;\n  let bravo = 0;\n  let charlie = 0;\n  for (const row of rows) alpha += bravo + charlie + row;\n  console.log(0);\n  console.log(1);",
  );

  expect(walls(source)).toHaveLength(1);
  expect(walls(source, "fix")).toHaveLength(1);

  const split = source.replace("  console.log(0);", "\n  console.log(0);");
  expect(walls(split)).toEqual([]);
  expect(formatText("wall.ts", split, { mode: "fix", braces: "off" }).fixed).toBeUndefined();
});

for (const forcedFirst of [true, false])
  test(`a separate six-call wall survives ${forcedFirst ? "after" : "before"} a forced group`, () => {
    const forced = guards(6);
    const statements = forcedFirst ? `${forced}\n\n${calls()}` : `${calls()}\n\n${forced}`;
    const source = body(statements);

    const expectedLine = forcedFirst ? 9 : 2;
    expect(walls(source)).toEqual([expect.objectContaining({ line: expectedLine })]);
    expect(walls(source, "fix")).toEqual([expect.objectContaining({ line: expectedLine })]);

    const split = source.replace("  console.log(3);", "\n  console.log(3);");
    expect(walls(split)).toEqual([]);
    expect(formatText("wall.ts", split, { mode: "fix", braces: "off" }).fixed).toBeUndefined();
  });

test("a forced group does not hide an adjacent clearable wall", () => {
  const source = body(`${guards(6)}\n  return "z";\n${calls()}`);
  expect(walls(source)).toEqual([expect.objectContaining({ line: 8 })]);
  expect(walls(source, "fix")).toEqual([expect.objectContaining({ line: 8 })]);

  const split = source.replace("  console.log(2);", "\n  console.log(2);");
  expect(walls(split)).toEqual([]);
  expect(formatText("wall.ts", split, { mode: "fix", braces: "off" }).fixed).toBeUndefined();
});

test("a forced group does not hide a preceding clearable wall", () => {
  const source = body(`${calls()}\n${guards(6)}`);
  expect(walls(source)).toEqual([expect.objectContaining({ line: 2 })]);
  expect(walls(source, "fix")).toEqual([expect.objectContaining({ line: 2 })]);
});

test("walls on both sides of a forced group remain separate findings", () => {
  const source = body(`${calls()}\n${guards(6)}\n\n${calls()}`);
  for (const mode of ["check", "fix"] as const)
    expect(walls(source, mode)).toEqual([
      expect.objectContaining({ line: 2 }),
      expect.objectContaining({ line: 15 }),
    ]);
});

test("hunk walls respect retained blank lines outside the changed scope", () => {
  const source = body(`${guards(6, true)}\n  return "z";\n${calls(2)}`);
  const options = {
    braces: "off" as const,
    changedLines: { lines: new Set([11, 12]), deletedAfter: new Set<number>() },
  };

  for (const mode of ["check", "fix"] as const) {
    const result = formatText("wall.ts", source, { ...options, mode });
    expect(result.parseError).toBe(false);
    expect(result.findings.filter((finding) => finding.rule === "wall")).toEqual([
      expect.objectContaining({ line: 6, fixable: false }),
    ]);

    expect(result.fixed).toBeUndefined();
  }
});

test("hunk walls recognize a forced group with untouched closed gaps", () => {
  for (const finalBlank of ["", "\n"]) {
    const source = body(`${guards(6)}\n${finalBlank}  return "z";`);
    const options = {
      braces: "off" as const,
      changedLines: { lines: new Set([4]), deletedAfter: new Set<number>() },
    };

    for (const mode of ["check", "fix"] as const) {
      const result = formatText("wall.ts", source, { ...options, mode });
      expect(result.parseError).toBe(false);
      expect(result.findings).toEqual([]);
      expect(result.fixed).toBeUndefined();
    }
  }
});

test("detached comments split forced groups and preserve clearable walls", () => {
  const source = body(
    "  if (ready) consume(0);\n  if (ready) consume(1);\n  if (ready) consume(2);\n  // section\n\n  if (ready) consume(3);\n  if (ready) consume(4);\n  if (ready) consume(5);\n  consume(6);\n  consume(7);\n  consume(8);",
  );

  for (const mode of ["check", "fix"] as const)
    expect(walls(source, mode)).toEqual([expect.objectContaining({ line: 7 })]);

  expect(formatText("wall.ts", source, { mode: "fix", braces: "off" }).fixed).toBeUndefined();

  const split = source.replace("  consume(6);", "\n  consume(6);");
  expect(walls(split)).toEqual([]);
  expect(formatText("wall.ts", split, { mode: "fix", braces: "off" }).fixed).toBeUndefined();
});

test("frozen gaps keep a wall beside a joined group actionable", () => {
  const source = body(`${guards(6)}\n\n  // stanza-ignore\n${calls(7)}`);
  expect(walls(source)).toEqual([expect.objectContaining({ line: 11 })]);
  expect(walls(source, "fix")).toEqual([expect.objectContaining({ line: 11 })]);
});

function fixed(source: string): string | undefined {
  return formatText("wall.ts", source, { mode: "fix", braces: "off" }).fixed;
}

function wallLines(source: string, mode: "check" | "fix" = "check"): number[] {
  return walls(source, mode).map((finding) => finding.line);
}

function codeIndexes(lines: string[]): number[] {
  return lines.flatMap((line, index) => (line.trimStart().startsWith("//") ? [] : [index]));
}

function keptSplits(lines: string[]): boolean[] {
  const code = codeIndexes(lines);
  return code
    .slice(1)
    .map((next, gap) =>
      Array.from({ length: next - code[gap]! }, (_, offset) => code[gap]! + 1 + offset).some(
        (at) => fixed(body(lines.toSpliced(at, 0, "").join("\n"))) === undefined,
      ),
    );
}

function clearableWalls(lines: string[]): number[] {
  const code = codeIndexes(lines);
  const kept = keptSplits(lines);
  const unsplittable = new Set<number>();

  let group = 0;
  for (let index = 1; index <= code.length; index++) {
    if (index < code.length && !kept[index - 1]) continue;
    if (index - group >= 6)
      for (let member = group; member < index; member++) unsplittable.add(member);

    group = index;
  }

  const found: number[] = [];

  let run = 0;
  for (let index = 0; index <= code.length; index++) {
    if (index < code.length && !unsplittable.has(index)) continue;
    if (index - run >= 6) found.push(code[run]! + 2);
    run = index + 1;
  }

  return found;
}

const chain = guards(6).split("\n");

const letLines = [
  "  let alpha = 0;",
  "  let bravo = 0;",
  "  let charlie = 0;",
  "  // note",
  "  let delta = 0;",
  "  let echo = 0;",
  "  let foxtrot = 0;",
  "  for (const row of rows) alpha += bravo + charlie + delta + echo + foxtrot + row;",
];

for (const [name, lines] of [
  ...[1, 2, 3, 4, 5].map(
    (above) =>
      [
        `a comment above guard ${above + 1} of six`,
        [...chain.slice(0, above), "  // note", ...chain.slice(above), '  return "z";'],
      ] as const,
  ),
  ["six guards and a return", [...chain, '  return "z";']],
  [
    "a comment above guard 2 of eight",
    [chain[0]!, "  // note", ...guards(8).split("\n").slice(1), '  return "z";'],
  ],
  ["a comment above the fourth of six lets read by one loop", letLines],
  ["a comment above the fourth of six calls", calls().split("\n").toSpliced(3, 0, "  // note")],
] as const)
  test(`wall reports exactly the runs a blank line --fix keeps can split: ${name}`, () => {
    const source = body(lines.join("\n"));
    expect(fixed(source)).toBeUndefined();

    const expected = clearableWalls([...lines]);
    expect(wallLines(source)).toEqual(expected);
    expect(wallLines(source, "fix")).toEqual(expected);

    for (let at = 1; at < lines.length; at++) {
      const split = body(lines.toSpliced(at, 0, "").join("\n"));
      if (fixed(split) !== source) continue;

      const shifted = expected.map((line) => (line >= at + 2 ? line + 1 : line));
      expect(wallLines(split)).toEqual(shifted);
    }
  });

test("a comment inside a guard chain leaves room for a blank line --fix keeps", () => {
  const commented = body(
    [...chain.slice(0, 3), "  // note", ...chain.slice(3), '  return "z";'].join("\n"),
  );

  for (const mode of ["check", "fix"] as const)
    expect(walls(commented, mode)).toEqual([expect.objectContaining({ line: 2, fixable: false })]);

  const apart = commented.replace("  // note\n", "  // note\n\n");
  expect(walls(apart)).toEqual([]);
  expect(fixed(apart)).toBeUndefined();

  const above = commented.replace("  // note\n", "\n  // note\n");
  expect(fixed(above)).toBe(commented);
  expect(walls(above)).toEqual([expect.objectContaining({ line: 2 })]);
});
