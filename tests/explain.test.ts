import { expect, test } from "bun:test";
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, relative } from "node:path";
import { cpuMs } from "./budget.ts";
import { batch, catalog, run, scratch } from "./support.ts";

const cwd = join(import.meta.dir, "..");
const { usage } = catalog();
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

test("explain measures an if/else by its widest arm", () => {
  const result = explained("tests/fixtures/block-spacing/else-arms-broken.after.ts:3");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain(
    "line 3 has 2 arms, the widest joins to 103 columns, too long for its own line",
  );
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

test.each([
  ["prettier.config.mjs", "export default { printWidth: 100 };\n", "100", "prettier.config.mjs"],
  ["prettier.config.mjs", "export default { printWidth: 80.5 };\n", "80", "prettier default"],
  ["prettier.config.mjs", "export default { printWidth: -5 };\n", "80", "prettier default"],
  ["prettier.config.mjs", 'export default { printWidth: "100" };\n', "80", "prettier default"],
  ["prettier.config.mjs", "export default { printWidth: Infinity };\n", "80", "prettier default"],
  ["prettier.config.mjs", "export default { printWidth: NaN };\n", "80", "prettier default"],
  ["prettier.config.mjs", "export default { printWidth: 1e21 };\n", "1e+21", "prettier.config.mjs"],
  [".editorconfig", "root = true\n[*]\nmax_line_length = 100\n", "100", ".editorconfig"],
  [".editorconfig", "root = true\n[*]\nmax_line_length = 80.5\n", "80", "prettier default"],
  [".editorconfig", "root = true\n[*]\nmax_line_length = 0x50\n", "80", ".editorconfig"],
])("explain prints the width %s gives as %j the way 0.4.1 does", (file, config, columns, from) => {
  const dir = scratch("explain-width-text");
  writeFileSync(join(dir, "package.json"), '{ "devDependencies": { "prettier": "3" } }');
  writeFileSync(join(dir, file), config);
  writeFileSync(
    join(dir, "a.ts"),
    "function f(value: string) {\n  if (!value)\n    return value;\n\n  if (value.length < 2)\n    return value;\n}\n",
  );

  const result = run({ cwd: dir }, "explain", "a.ts:5");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain(`width:     ${columns} columns, from ${from}\n`);
  expect(result.stdout).toContain(`both guards fit on one line at ${columns} columns\n`);
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
    "stanza: cannot read the line width from prettier.config.mjs, so the next source sets it",
  );
});

test("explain names the lint config file that keeps the braces", () => {
  const result = explained("tests/fixtures/braces-enforced/kept.before.ts:3");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain(
    "kept by:   tests/fixtures/braces-enforced/biome.json enforces braces",
  );
});

test("explain names why it cannot read the lint config that decides the braces", () => {
  const dir = scratch("explain-unread-config");
  const fixture = join(cwd, "tests/fixtures/braces-enforced");
  writeFileSync(join(dir, "kept.ts"), readFileSync(join(fixture, "kept.before.ts")));
  writeFileSync(join(dir, "biome.json"), '{ "extends": ["./base.js"] }');
  writeFileSync(
    join(dir, "base.js"),
    'const a = { x: b, style: { useBlockStatements: "off" } }; const b = { y: a };\nexport default a;\n',
  );

  expect(run({ cwd: dir }, "explain", "kept.ts:3").stdout).toContain(
    "kept by:   stanza cannot read the lint config: a config value contains itself, so it keeps them",
  );
});

test("explain removes the braces once no lint config keeps them", () => {
  const fixture = join(cwd, "tests/fixtures/braces-enforced");
  const configured = scratch("explain-configured");
  const unconfigured = scratch("explain-unconfigured");
  for (const dir of [configured, unconfigured])
    writeFileSync(join(dir, "kept.ts"), readFileSync(join(fixture, "kept.before.ts")));

  writeFileSync(join(configured, "biome.json"), readFileSync(join(fixture, "biome.json")));

  expect(run({ cwd: configured }, "explain", "kept.ts:3").stdout).toContain(
    "result:    the braces stay",
  );

  expect(run({ cwd: unconfigured }, "explain", "kept.ts:3").stdout).toContain(
    "result:    --fix removes the braces",
  );
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

test.each([
  ["const value = 1;\nvalue;\n", 3, "a.ts has 2 lines, not 3"],
  ["const value = 1;\nvalue;\n", 99, "a.ts has 2 lines, not 99"],
  ["const value = 1;\r\nvalue;\r\n", 3, "a.ts has 2 lines, not 3"],
  ["const value = 1;\nvalue;", 3, "a.ts has 2 lines, not 3"],
  ["", 1, "a.ts has 0 lines, not 1"],
  ["const value = 1;\n", 2, "a.ts has 1 line, not 2"],
])("explain counts the lines of %j like an editor, rejecting line %i", (text, line, problem) => {
  const dir = scratch("explain-line-count");
  writeFileSync(join(dir, "a.ts"), text);

  expect(run({ cwd: dir }, "explain", `a.ts:${line}`)).toEqual({
    code: 2,
    stderr: `stanza: ${problem}\n`,
    stdout: "",
  });
});

test("explain names a line past 2^53 as it was typed", () => {
  const dir = scratch("explain-huge-line");
  writeFileSync(join(dir, "a.ts"), "const value = 1;\n");

  expect(run({ cwd: dir }, "explain", "a.ts:9007199254740993")).toEqual({
    code: 2,
    stderr: "stanza: a.ts has 1 line, not 9007199254740993\n",
    stdout: "",
  });
});

test("explain reads the last line the same with or without a final newline", () => {
  const dir = scratch("explain-final-newline");
  writeFileSync(join(dir, "a.ts"), "const value = 1;\nvalue;\n");
  writeFileSync(join(dir, "b.ts"), "const value = 1;\nvalue;");

  const unended = run({ cwd: dir }, "explain", "b.ts:2");
  expect(unended.code).toBe(0);
  expect(run({ cwd: dir }, "explain", "a.ts:2")).toEqual({
    ...unended,
    stdout: unended.stdout.replace("b.ts:2", "a.ts:2"),
  });
});

test("explain accepts a leading dash location after --", () => {
  const cwd = scratch("explain-dashdash");
  writeFileSync(join(cwd, "-dash.ts"), "if (ready) {\n  work();\n}\n");

  const expected = run({ cwd }, "explain", "./-dash.ts:1");
  expect(expected.code).toBe(0);
  expect(run({ cwd }, "explain", "--", "-dash.ts:1")).toEqual(expected);
});

test.each([
  [[], "explain needs <file>:<line>"],
  [["a.ts:1", "b.ts:2"], "unexpected argument b.ts:2"],
  [["--no-braces"], "explain needs <file>:<line>"],
  [["a.ts:1", "--no-braces"], "unexpected argument --no-braces"],
  [["--", "a.ts:1"], "unexpected argument --"],
  [["a.ts:0"], "a.ts:0: the line must be 1 or more"],
])("explain rejects invalid locations after --: %j", (locations, problem) => {
  expect(run("explain", "--", ...locations)).toEqual({
    code: 2,
    stderr: `stanza: ${problem}\n${usage}\n`,
    stdout: "",
  });
});

test("explain recognizes --no-braces only before --", () => {
  const cwd = scratch("explain-no-braces");
  writeFileSync(join(cwd, "--no-braces.ts"), "if (ready) {\n  work();\n}\n");
  writeFileSync(join(cwd, "-dash.ts"), "if (ready) {\n  work();\n}\n");

  const expected = run({ cwd }, "explain", "./-dash.ts:1", "--no-braces");
  expect(expected.code).toBe(0);
  expect(expected.stdout).toContain("--no-braces");
  expect(run({ cwd }, "explain", "--no-braces", "--", "-dash.ts:1")).toEqual(expected);
  expect(run({ cwd }, "explain", "--", "--no-braces.ts:1")).toEqual(
    run({ cwd }, "explain", "./--no-braces.ts:1"),
  );
});

test.each([
  [["--no-braces", "--no-braces", "a.ts:1"], "--no-braces given twice"],
  [["-dash.ts:1"], "unexpected argument -dash.ts:1"],
  [["a.ts:1", "b.ts:2"], "unexpected argument b.ts:2"],
])("explain preserves usage errors without --: %j", (args, problem) => {
  expect(run("explain", ...args)).toEqual({
    code: 2,
    stderr: `stanza: ${problem}\n${usage}\n`,
    stdout: "",
  });
});

test("explain preserves exit 1 after -- when nothing is explained", () => {
  const cwd = scratch("explain-no-gap");
  writeFileSync(join(cwd, "-dash.ts"), "work();\n");

  const expected = run({ cwd }, "explain", "./-dash.ts:1");
  expect(expected.code).toBe(1);
  expect(run({ cwd }, "explain", "--", "-dash.ts:1")).toEqual(expected);
});

test.each([
  [
    "function h() {\n  if (a) {\n    return {}\n  }\n  (g)();\n}\n",
    2,
    "kept by:   `return {}` has no semicolon, so without braces it could continue onto `(g)();` on line 5",
  ],
  [
    "if (x) {\n  go();\n} // keep\n",
    1,
    "kept by:   a comment after the closing brace on line 3 would move onto the next line without it",
  ],
  [
    "if (x) {\n  // c\n  y();\n}\n",
    1,
    "kept by:   a comment on line 2 sits inside the braces but outside the statement",
  ],
  [
    "if (a) {\n  if (b) c();\n} else d();\n",
    1,
    "kept by:   without braces, the `else` on line 3 would attach to the `if` inside them",
  ],
  ["do{x();}while(a);\n", 1, "kept by:   removing the braces would run two words together"],
  [
    "if (y) { let z = 1; }\n",
    1,
    "kept by:   the body is a `let` statement, and only an expression, `return`, `throw`, `break`, `continue`, `if` or loop stands without braces",
  ],
  ["if (s) { a(); b(); }\n", 1, "rule:      braces does not apply, the body holds 2 statements"],
  ["if (r) {}\n", 1, "rule:      braces does not apply, the body is empty"],
  [
    "// stanza-ignore\nfor (;;) {\n  k();\n}\n",
    2,
    "rule:      braces, but a stanza directive covers this body",
  ],
  [
    "// stanza-off\nif (v) {\n  w();\n}\n// stanza-on\n",
    3,
    "line 3 is inside a stanza-off region, so stanza leaves it alone",
  ],
  [
    "function f() {\n  const response = get();\n\n  if (!response) return;\n}\n",
    4,
    "outranked: guard-join, wants no blank line: `response` is bound on line 2 and read by the `if` below it",
  ],
])("explain outside a repository on %j line %i says %s", (source, line, expected) => {
  const dir = scratch("explain-reason");
  writeFileSync(join(dir, "a.ts"), source);

  const result = run({ cwd: dir }, "explain", `a.ts:${line}`);
  expect(result.code).toBe(0);
  expect(result.stdout).toContain(`${expected}\n`);
});

test("explain measures the widest arm outside a repository", () => {
  const dir = scratch("explain-arms");
  writeFileSync(
    join(dir, "a.ts"),
    readFileSync(join(cwd, "tests/fixtures/block-spacing/else-arms-broken.after.ts")),
  );

  expect(run({ cwd: dir }, "explain", "a.ts:3").stdout).toContain(
    "line 3 has 2 arms, the widest joins to 103 columns, too long for its own line",
  );
});

test("explain usage shows the delimiter after options", () => {
  expect(run("--help").stdout).toContain("stanza explain [--no-braces] [--] <file>:<line>");
});

interface StatedGap {
  line: number;
  result: string;
}

function contentKey(line: string): string {
  return line.replace(/[\s{}]/g, "");
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
  let at = 0;
  let joined = "";
  for (const [index, entry] of before.slice(0, line).entries()) {
    const key = contentKey(entry);
    if (key === "") continue;

    while (at < after.length && contentKey(after[at]!) === "") at++;
    if (index + 1 === line) return at + 1;

    joined += key;
    const target = contentKey(after[at] ?? "");
    if (!target.startsWith(joined)) break;

    if (joined === target) {
      joined = "";
      at++;
    }
  }

  throw new Error(`no line in the after file matches line ${line}`);
}

function mayEndGap(line: string): boolean {
  return !/^[\s})\],]*$/.test(line);
}

function statedGaps(explanations: Record<string, string>): StatedGap[] {
  const gaps = new Map<number, string>();
  for (const explained of Object.values(explanations))
    for (const section of explained.split("\n\n")) {
      const [heading, above] = section.split("\n");
      const next = /^gap above line (\d+),/.exec(heading!)?.[1];
      const previousEnd = /^ +(?:\d+-)?(\d+) /.exec(above ?? "")?.[1];
      if (next === undefined || previousEnd === next) continue;

      const result = /^result: +(.*)$/m.exec(section)![1]!;
      gaps.set(Number(next), result);
    }

  return [...gaps].map(([line, result]) => ({ line, result }));
}

test("explain states what --fix does to every fixture gap", () => {
  const started = cpuMs();
  const root = join(import.meta.dir, "fixtures");
  const pairs = readdirSync(root).flatMap((dir) =>
    readdirSync(join(root, dir)).flatMap((file) => {
      const match = /^(.+)\.before\.(tsx?|jsx?)$/.exec(file);
      if (!match) return [];

      const path = join(root, dir, file);
      const text = readFileSync(path, "utf8");
      const after = readFileSync(join(root, dir, `${match[1]}.after.${match[2]}`), "utf8");
      return [{ dir, file, path, text, before: text.split("\n"), after: after.split("\n") }];
    }),
  );

  const answers = batch(
    cwd,
    pairs.map(({ path, text, before }) => ({
      path: relative(cwd, path),
      text,
      mode: "check" as const,
      braces: null,
      changedLines: null,
      explain: before.flatMap((line, index) => (mayEndGap(line) ? [index + 1] : [])),
    })),
  );

  for (const [index, { dir, file, before, after }] of pairs.entries()) {
    const answer = answers[index]!;
    expect(answer.error).toBeNull();

    for (const [line, explained] of Object.entries(answer.explanations))
      expect(explained, `${dir}/${file}:${line}`).toStartWith(`${file}:${line}`);

    for (const gap of statedGaps(answer.explanations)) {
      const was = blankLinesAbove(before, gap.line);
      const is = blankLinesAbove(after, matchingLine(before, after, gap.line));
      const where = `${dir}/${file}:${gap.line} says ${gap.result}, blank lines ${was} then ${is}`;
      if (gap.result.startsWith("--fix removes")) expect([was > 0, is], where).toEqual([true, 0]);
      else if (gap.result === "--fix adds a blank line") expect([was, is], where).toEqual([0, 1]);
      else expect(is, where).toBe(was);
    }
  }

  expect(cpuMs() - started).toBeLessThan(5_000);
}, 60_000);
