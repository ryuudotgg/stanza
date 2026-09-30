import { expect, test } from "bun:test";
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { explain } from "../src/engine/explain.ts";
import { languageOf } from "../src/languages/index.ts";
import { run, scratch } from "./support.ts";

const cwd = join(import.meta.dir, "..");

function explained(target: string, ...flags: string[]) {
  return run({ cwd }, "explain", target, ...flags);
}

test("explain names the join rule and the bound name", () => {
  const result = explained("tests/fixtures/guard-join/guards.before.ts:4");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain("rule:      guard-join, wants no blank line");
  expect(result.stdout).toContain("`response` is bound on line 2 and read by the `if` below it");
});

test("explain names the width and joined columns for long guards", () => {
  const result = explained("tests/fixtures/guard-chain/too-long.before.ts:4");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain("width:     100 columns, from oxfmt default");
  expect(result.stdout).toContain("line 2 joins to 174 columns, too long for one line");
});

test("explain names a config source and a fitting joined guard", () => {
  const dir = scratch("explain-width");
  writeFileSync(join(dir, ".oxfmtrc.json"), '{ "printWidth": 120 }');
  writeFileSync(
    join(dir, "a.ts"),
    "function f(value: string) {\n  if (!value)\n    return value;\n\n  if (value.length < 2)\n    return value;\n}\n",
  );

  const result = run({ cwd: dir }, "explain", "a.ts:5");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain("width:     120 columns, from .oxfmtrc.json");
  expect(result.stdout).toContain("fits on one line");
  expect(result.stdout).toContain("both guards fit on one line at 120 columns");
});

test("explain calls one line guards compact and short bodies compact", () => {
  const dir = scratch("explain-compact");
  writeFileSync(
    join(dir, "a.ts"),
    "function f(a: boolean) {\n  if (a) return;\n\n  if (!a) return;\n}\nfunction g(a: boolean) {\n  const b = a;\n\n  if (b) return;\n}\n",
  );

  expect(run({ cwd: dir }, "explain", "a.ts:4").stdout).toContain(
    "both statements are compact guards",
  );

  expect(run({ cwd: dir }, "explain", "a.ts:9").stdout).toContain(
    "the block holds 2 statements, each compact",
  );
});

test("the width notice says the next source was used", () => {
  const dir = scratch("explain-unread-width");
  writeFileSync(join(dir, "package.json"), '{ "devDependencies": { "prettier": "3" } }');
  writeFileSync(
    join(dir, "prettier.config.mjs"),
    "export default { printWidth: Number(process.env.W ?? 120) };\n",
  );

  writeFileSync(
    join(dir, "a.ts"),
    "function f(a: boolean) {\n  if (a) return;\n\n  if (!a) return;\n}\n",
  );

  const result = run({ cwd: dir }, "--check", "a.ts");
  expect(result.stderr).toContain(
    "stanza: could not read the line width from prettier.config.mjs, so stanza used the next source for it",
  );
});

test("explain names the lint config file that keeps the braces", () => {
  const result = explained("tests/fixtures/braces-enforced/kept.before.ts:3");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain(
    "kept by:   tests/fixtures/braces-enforced/biome.json enforces braces",
  );
});

test("explain with braces forced on removes braces a lint config would keep", () => {
  const path = join(cwd, "tests/fixtures/braces-enforced/kept.before.ts");
  const text = readFileSync(path, "utf8");
  const request = {
    language: languageOf(path),
    path,
    text,
    line: 3,
    display: (file: string) => file,
  };

  const forced = explain({ ...request, braces: "on" });
  const configured = explain({ ...request, braces: undefined });
  if ("error" in forced || "error" in configured) throw new Error("expected explanations");

  expect(configured.lines).toContain("result:    the braces stay");
  expect(forced.lines).toContain("result:    --fix removes the braces");
});

test("explain says a missing semicolon keeps the braces", () => {
  const result = explained("tests/fixtures/braces/asi.before.ts:2");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain(
    "`return {}` has no semicolon, so without braces it could continue onto `(g)();` on line 5",
  );

  expect(result.stdout).toContain("result:    the braces stay");
});

test("explain says a closing comment keeps the braces", () => {
  const result = explained("tests/fixtures/braces/closing-comment.before.ts:18");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain(
    "a comment after the closing brace on line 20 would move onto the next line without it",
  );

  expect(result.stdout).toContain("result:    the braces stay");
});

test("explain says a directive keeps a labelled loop's braces", () => {
  const result = explained("tests/fixtures/ignore/labels.before.ts:3");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain("rule:      braces, but a stanza directive covers this body");
  expect(result.stdout).not.toContain("--fix removes the braces");
});

test("explain lists the rules the deciding rule outranked", () => {
  const dir = scratch("explain");
  writeFileSync(
    join(dir, "a.ts"),
    "function f(a: number) {\n  const b = a;\n\n  if (!b) return;\n}\n",
  );

  const result = run({ cwd: dir }, "explain", "a.ts:4");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain("rule:      short-body, wants no blank line");
  expect(result.stdout).toContain("outranked: guard-join, wants no blank line");
  expect(result.stdout).not.toContain("note:");
});

test("explain with --no-braces says the flag keeps the braces", () => {
  const dir = scratch("explain");
  writeFileSync(join(dir, "a.ts"), "if (x) {\n  go();\n}\n");

  expect(run({ cwd: dir }, "explain", "a.ts:1").stdout).toContain("--fix removes the braces");
  expect(run({ cwd: dir }, "explain", "a.ts:1", "--no-braces").stdout).toContain(
    "kept by:   --no-braces turns the rule off",
  );
});

test("explain exits 1 on a line with nothing to explain and 2 on bad input", () => {
  const dir = scratch("explain");
  writeFileSync(join(dir, "a.ts"), "go();\n");

  expect(run({ cwd: dir }, "explain", "a.ts:1").code).toBe(1);

  expect(run({ cwd: dir }, "explain", "a.ts:9").code).toBe(2);
  expect(run({ cwd: dir }, "explain", "a.ts").code).toBe(2);
  expect(run({ cwd: dir }, "explain", "a.ts:1", "--check").code).toBe(2);
  expect(run({ cwd: dir }, "explain", "a.ts:1", "b.ts:1").code).toBe(2);
  expect(run({ cwd: dir }, "explain", "a.ts:1", "--no-braces", "--no-braces").code).toBe(2);
});

test("explain follows --fix through nested brace removal", () => {
  const dir = scratch("explain");
  const source =
    "function a(x: boolean) {\n  if (x) {\n    foo();\n  }\n\n  bar();\n}\n\nfunction b(p: boolean, q: boolean) {\n  if (p) {\n    while (q) {\n      step();\n    }\n  } else stop();\n}\n";

  writeFileSync(join(dir, "a.ts"), source);

  const gap = run({ cwd: dir }, "explain", "a.ts:6").stdout;
  expect(gap).toContain("note:      decided after --fix removes the braces on line 2");
  expect(gap).toContain("rule:      short-body, wants no blank line");

  expect(run({ cwd: dir }, "explain", "a.ts:10").stdout).toContain(
    "result:    --fix removes the braces once the braces inside them are gone",
  );
});

test("explain names a decorated class body without a leading word", () => {
  const dir = scratch("explain");
  writeFileSync(join(dir, "a.ts"), "if (x) { @foo class X {} }\n");

  const result = run({ cwd: dir }, "explain", "a.ts:1");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain("the body is a class declaration");
});

interface StatedGap {
  line: number;
  result: string;
}

function contentLine(line: string): boolean {
  return line.replace(/[{}]/g, "").trim() !== "";
}

function blankLinesAbove(lines: string[], line: number): number {
  let count = 0;
  for (let index = line - 2; index >= 0; index--) {
    const content = lines[index]!.trim();
    if (content === "") count++;
    else if (!/^(\/\/|\/\*|\*)/.test(content)) break;
  }

  return count;
}

function matchingLine(before: string[], after: string[], line: number): number {
  const ordinal = before.slice(0, line).filter(contentLine).length;

  let seen = 0;
  for (const [index, entry] of after.entries())
    if (contentLine(entry) && ++seen === ordinal) return index + 1;

  throw new Error(`no line in the after file matches line ${line}`);
}

function statedGaps(path: string, text: string): StatedGap[] {
  const gaps = new Map<number, string>();
  for (let line = 1; line <= text.split("\n").length; line++) {
    const explained = explain({
      language: languageOf(path),
      path,
      text,
      line,
      braces: undefined,
      display: (file) => file,
    });

    if ("error" in explained) throw new Error(explained.error);

    for (const section of explained.lines.join("\n").split("\n\n")) {
      const [heading, above] = section.split("\n");
      const next = /^gap above line (\d+),/.exec(heading!)?.[1];
      const previousEnd = /^ +(?:\d+-)?(\d+) /.exec(above ?? "")?.[1];
      if (next === undefined || previousEnd === next) continue;

      const result = /^result: +(.*)$/m.exec(section)![1]!;
      gaps.set(Number(next), result);
    }
  }

  return [...gaps].map(([line, result]) => ({ line, result }));
}

test("explain states what --fix does to every fixture gap", () => {
  const root = join(import.meta.dir, "fixtures");
  for (const dir of readdirSync(root))
    for (const file of readdirSync(join(root, dir))) {
      const match = /^(.+)\.before\.(tsx?|jsx?)$/.exec(file);
      if (!match) continue;

      const path = join(root, dir, file);
      const text = readFileSync(path, "utf8");
      const before = text.split("\n");
      const after = readFileSync(join(root, dir, `${match[1]}.after.${match[2]}`), "utf8").split(
        "\n",
      );

      for (const gap of statedGaps(path, text)) {
        const was = blankLinesAbove(before, gap.line);
        const is = blankLinesAbove(after, matchingLine(before, after, gap.line));
        const where = `${dir}/${file}:${gap.line} says ${gap.result}, blank lines ${was} then ${is}`;
        if (gap.result.startsWith("--fix removes")) expect([was > 0, is], where).toEqual([true, 0]);
        else if (gap.result === "--fix adds a blank line") expect([was, is], where).toEqual([0, 1]);
        else expect(is, where).toBe(was);
      }
    }
});
