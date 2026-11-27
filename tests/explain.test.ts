import { expect, test } from "bun:test";
import { cpSync, mkdirSync, readdirSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import { join, relative } from "node:path";
import catalog from "../docs/lib/catalog.json";
import { cpuMs } from "./budget.ts";
import { batch, run, scratch, scratchGitRepository } from "./support.ts";

const cwd = join(import.meta.dir, "..");
const pointer = "run `stanza --help` for usage";
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
  expect(result.stdout).toContain("width:     100 columns, from tests/fixtures/.oxfmtrc.json");
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
  [
    "prettier.config.mjs",
    "export default { printWidth: 1e21 };\n",
    "1000000000000000000000",
    "prettier.config.mjs",
  ],
  [".editorconfig", "root = true\n[*]\nmax_line_length = 100\n", "100", ".editorconfig"],
  [".editorconfig", "root = true\n[*]\nmax_line_length = 80.5\n", "80", "prettier default"],
  [".editorconfig", "root = true\n[*]\nmax_line_length = 0x50\n", "80", "prettier default"],
])("explain prints the width %s gives as %j in whole digits", (file, config, columns, from) => {
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

test("explain prints an .editorconfig max_line_length = off as unlimited", () => {
  const dir = scratch("explain-width-off");
  writeFileSync(join(dir, ".prettierrc.json"), "{}");
  writeFileSync(join(dir, ".editorconfig"), "root = true\n[*]\nmax_line_length = off\n");
  writeFileSync(
    join(dir, "a.ts"),
    "function f(value: string) {\n  if (!value)\n    return value;\n\n  if (value.length < 2)\n    return value;\n}\n",
  );

  const result = run({ cwd: dir }, "explain", "a.ts:5");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain("width:     unlimited, from .editorconfig\n");
  expect(result.stdout).toContain("both guards fit on one line, the width is unlimited\n");
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

  expect(run({ cwd: dir }, "explain", "a.ts:4").stdout).toContain(
    "note:      cannot read the line width from prettier.config.mjs\n",
  );
});

test("explain names the lint config file that keeps the braces", () => {
  const result = explained("tests/fixtures/braces-enforced/kept.before.ts:3");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain(
    "kept by:   tests/fixtures/braces-enforced/biome.json enforces braces",
  );
});

test("explain names the Biome config whose JavaScript base it cannot read", () => {
  const dir = scratch("explain-unread-config");
  const fixture = join(cwd, "tests/fixtures/braces-enforced");
  writeFileSync(join(dir, "kept.ts"), readFileSync(join(fixture, "kept.before.ts")));
  writeFileSync(join(dir, "biome.json"), '{ "extends": ["./base.js"] }');
  writeFileSync(
    join(dir, "base.js"),
    'export default { linter: { rules: { style: { useBlockStatements: "off" } } } };\n',
  );

  expect(run({ cwd: dir }, "explain", "kept.ts:3").stdout).toContain(
    "kept by:   stanza cannot tell whether biome.json enforces braces, so it keeps them",
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

test("explain with --braces agrees with a --check --braces finding", () => {
  const dir = scratch("explain-braces");
  writeFileSync(join(dir, "a.ts"), "if (a) {\n  b();\n}\n");
  writeFileSync(join(dir, ".oxlintrc.json"), '{"rules":{"curly":"error"}}');

  const check = run({ cwd: dir }, "--check", "--braces", "a.ts");
  expect(check.code).toBe(1);
  expect(check.stdout.match(/: braces: /g)).toHaveLength(1);

  const forced = run({ cwd: dir }, "explain", "--braces", "a.ts:1");
  expect(forced.code).toBe(0);
  expect(forced.stdout).toContain("result:    --fix removes the braces");
  expect(forced.stdout).not.toContain("the braces stay");
  expect(forced.stdout).not.toContain(".oxlintrc.json enforces braces");

  expect(run({ cwd: dir }, "explain", "a.ts:1").stdout).toContain("result:    the braces stay");
  expect(run({ cwd: dir }, "explain", "--no-braces", "a.ts:1").stdout).toContain(
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

test("explain names each removed body by its opening brace line, once and ascending", () => {
  const dir = scratch("explain-brace-lines");
  const sources = {
    "allman.ts": "function f() {\n  if (a)\n  {\n    b();\n  }\n  c();\n}\n",
    "nested.ts":
      "function f() {\n  if (a)\n  {\n    if (b)\n    {\n      c();\n    }\n  }\n  d();\n}\n",
    "sameline.ts": "function f() {\n  if (a) { if (b) { c(); } }\n  d();\n}\n",
    "nestedkr.ts": "function f() {\n  if (a) {\n    if (b) {\n      c();\n    }\n  }\n  d();\n}\n",
  };

  for (const [name, source] of Object.entries(sources)) writeFileSync(join(dir, name), source);

  const note = (target: string) =>
    run({ cwd: dir }, "explain", target)
      .stdout.split("\n")
      .find((line) => line.startsWith("note:"));

  expect(run({ cwd: dir }, "--check", "allman.ts").stdout).toContain("allman.ts:3:3: braces");
  expect(note("allman.ts:6")).toBe("note:      decided after --fix removes the braces on line 3");
  expect(note("nested.ts:9")).toBe(
    "note:      decided after --fix removes the braces on lines 3 and 5",
  );

  expect(note("sameline.ts:3")).toBe("note:      decided after --fix removes the braces on line 2");
  expect(note("nestedkr.ts:7")).toBe(
    "note:      decided after --fix removes the braces on lines 2 and 3",
  );
});

test("explain prints the wall a line runs through", () => {
  const dir = scratch("explain-wall");
  const calls = ["a", "b", "c", "d", "e", "g", "h", "i"].map((call) => `  ${call}();\n`);
  writeFileSync(join(dir, "a.ts"), `function f() {\n${calls.join("")}}\n`);

  const section = [
    "wall, lines 2 to 9",
    "  2        a();",
    "  9        i();",
    "rule:      wall, wants a blank line where one step ends",
    "because:   8 single-line statements run with no blank line --fix keeps, and 6 or more make a wall",
    "result:    --fix leaves the split to you",
  ].join("\n");

  expect(run({ cwd: dir }, "--check", "a.ts").stdout).toStartWith(
    "a.ts:2:3: wall: split these 8 statements",
  );

  const first = run({ cwd: dir }, "explain", "a.ts:2");
  expect([first.code, first.stdout]).toEqual([0, `a.ts:2\n\n${section}\n`]);

  const inside = run({ cwd: dir }, "explain", "a.ts:5");
  expect(inside.code).toBe(0);
  expect(inside.stdout).toStartWith("a.ts:5\n\ngap above line 5, no blank lines\n");
  expect(inside.stdout).toEndWith(`\n\n${section}\n`);
});

test("explain counts a wall as --fix leaves it", () => {
  const dir = scratch("explain-wall-joined");
  writeFileSync(
    join(dir, "a.ts"),
    "function f() {\n  a();\n  b();\n  c();\n  d();\n  const x = get();\n\n  if (!x) return;\n}\n",
  );

  expect(run({ cwd: dir }, "--check", "a.ts").stdout).toStartWith(
    "a.ts:2:3: wall: split these 6 statements",
  );

  const result = run({ cwd: dir }, "explain", "a.ts:2");
  expect(result.code).toBe(0);
  expect(result.stdout).toContain("\nwall, lines 2 to 8\n");
  expect(result.stdout).toContain(
    "\nbecause:   6 single-line statements run with no blank line --fix keeps, and 6 or more make a wall\n",
  );
});

test("explain prints a wall after the braced body on its line", () => {
  const dir = scratch("explain-wall-braces");
  writeFileSync(
    join(dir, "a.ts"),
    "function f() {\n  a();\n  b();\n  c();\n  d();\n  e();\n  if (x) {\n    y();\n  }\n}\n",
  );

  expect(run({ cwd: dir }, "--check", "a.ts").stdout).toStartWith(
    "a.ts:2:3: wall: split these 6 statements",
  );

  const result = run({ cwd: dir }, "explain", "a.ts:7");
  const headings = result.stdout.split("\n\n").map((section) => section.split("\n")[0]);
  expect(headings).toEqual([
    "a.ts:7",
    "gap above line 7, no blank lines",
    "braced body, lines 7 to 9",
    "wall, lines 2 to 9",
  ]);

  expect(result.stdout).toContain(
    "\nwall, lines 2 to 9\n  2        a();\nnote:      decided after --fix removes the braces on line 7\nrule:      wall,",
  );

  expect(result.stdout.split("\n").filter((line) => line.endsWith("if (x) {"))).toEqual([
    "  7        if (x) {",
  ]);
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

test("explain names a line past every integer as it was typed", () => {
  const dir = scratch("explain-overflow-line");
  writeFileSync(join(dir, "a.ts"), "const a = 1;\nconst b = 2;\nconst c = 3;\nconst d = 4;\n");

  expect(run({ cwd: dir }, "explain", "a.ts:99999999999999999999999")).toEqual({
    code: 2,
    stderr: "stanza: a.ts has 4 lines, not 99999999999999999999999\n",
    stdout: "",
  });
});

test("explain reads every finding's path:line:col as its path:line", () => {
  const dir = scratch("explain-finding-locations");
  cpSync(join(cwd, "tests/fixtures"), join(dir, "fixtures"), { recursive: true });

  const checked = run({ cwd: dir }, "--check", "--json", "fixtures");
  const findings = JSON.parse(checked.stdout) as {
    path: string;
    line: number;
    col: number;
    rule: string;
  }[];

  expect(new Set(findings.map((finding) => finding.rule))).toEqual(
    new Set(Object.keys(catalog.rules)),
  );

  for (const { path, line, col } of findings) {
    const located = run({ cwd: dir }, "explain", `${path}:${line}:${col}`);
    expect({ path, line, col, ...located }).toEqual({
      path,
      line,
      col,
      ...run({ cwd: dir }, "explain", `${path}:${line}`),
    });
  }
});

test("explain ignores the column, 0 and a missing file included", () => {
  const dir = scratch("explain-column");
  writeFileSync(
    join(dir, "a.ts"),
    "function f() {\n  const a = g();\n  if (!a) return;\n  b();\n}\n",
  );

  expect(run({ cwd: dir }, "explain", "a.ts:2:0")).toEqual(run({ cwd: dir }, "explain", "a.ts:2"));
  expect(run({ cwd: dir }, "explain", "nope.ts:2:3")).toEqual({
    ...run({ cwd: dir }, "explain", "nope.ts:2"),
    code: 2,
  });
});

test.each([
  ["missing\nfile.ts:1", 'no such file: "missing\\nfile.ts"'],
  ["n\nl.txt:1", 'not a TypeScript or JavaScript file: "n\\nl.txt"'],
  ["d\nir.ts:1", 'not a regular file: "d\\nir.ts"'],
])("explain quotes a path holding a line break in its error: %j", (target, problem) => {
  const dir = scratch("explain-quoted-errors");
  mkdirSync(join(dir, "d\nir.ts"));

  expect(run({ cwd: dir }, "explain", target)).toEqual({
    code: 2,
    stderr: `stanza: ${problem}\n`,
    stdout: "",
  });
});

test("explain takes a path holding a line terminator and quotes it in the header", () => {
  const dir = scratch("explain-terminators");
  const source = "function f() {\n  const a = g();\n  if (!a) return;\n  b();\n}\n";
  for (const name of ["ls.ts", "l s.ts", "n\nl.ts"]) writeFileSync(join(dir, name), source);

  const plain = run({ cwd: dir }, "explain", "ls.ts:4");
  expect(plain.stdout).toStartWith("ls.ts:4\n");

  const named = (shown: string) => ({ ...plain, stdout: plain.stdout.replace("ls.ts:4", shown) });
  expect(run({ cwd: dir }, "explain", "l s.ts:4")).toEqual(named('"l\\342\\200\\250s.ts":4'));
  expect(run({ cwd: dir }, "explain", "n\nl.ts:4")).toEqual(named('"n\\nl.ts":4'));
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
  [[], "explain needs <file>:<line>[:<col>]"],
  [["a.ts:1", "b.ts:2"], "unexpected argument b.ts:2"],
  [["--no-braces"], "explain needs <file>:<line>[:<col>]"],
  [["a.ts:1", "--no-braces"], "unexpected argument --no-braces"],
  [["--", "a.ts:1"], "unexpected argument --"],
  [["a.ts:0"], "a.ts:0: the line must be 1 or more"],
])("explain rejects invalid locations after --: %j", (locations, problem) => {
  expect(run("explain", "--", ...locations)).toEqual({
    code: 2,
    stderr: `stanza: ${problem}\n${pointer}\n`,
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
  [["--braces", "--braces", "a.ts:1"], "--braces given twice"],
  [["--braces", "--no-braces", "a.ts:1"], "use one of --braces or --no-braces"],
  [["--brace", "a.ts:1"], "unknown flag --brace, did you mean --braces?"],
  [["-dash.ts:1"], "unknown flag -dash.ts:1"],
  [["a.ts:1", "b.ts:2"], "unexpected argument b.ts:2"],
  [["--no-brace", "a.ts:1"], "unknown flag --no-brace, did you mean --no-braces?"],
  [["a.ts:1", "--", "--no-brace"], "unexpected argument --no-brace"],
  [["a.ts:1", "b\n.ts:2"], 'unexpected argument "b\\n.ts:2"'],
  [[], "explain needs <file>:<line>[:<col>]"],
])("explain preserves usage errors without --: %j", (args, problem) => {
  expect(run("explain", ...args)).toEqual({
    code: 2,
    stderr: `stanza: ${problem}\n${pointer}\n`,
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
    "function f() {\n  if (a) {\n    g()\n  }}\n",
    2,
    "kept by:   the closing brace on line 4 shares its line with the `}` after it, which would be left behind without it",
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
  expect(run("--help").stdout).toContain(
    "stanza explain [--braces | --no-braces] [--] <file>:<line>[:<col>]",
  );
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
      if (next === undefined || previousEnd === undefined || previousEnd === next) continue;

      const result = /^result: +(.*)$/m.exec(section)![1]!;
      gaps.set(Number(next), result);
    }

  return [...gaps].map(([line, result]) => ({ line, result }));
}

test("explain states what --fix does to every fixture gap", () => {
  const started = cpuMs();
  const root = join(import.meta.dir, "fixtures");
  const dirs = readdirSync(root, { withFileTypes: true }).filter((entry) => entry.isDirectory());
  const pairs = dirs.flatMap(({ name: dir }) =>
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

const singleBlock = "if (a) {\n  b();\n}\n";

const braced = (name: string) => `if (${name}) {\n  ${name}();\n}\n`;

const bracesAt = (path: string) =>
  `${path}:1:8: braces: remove the braces around this single statement body\n`;

const symlinkLoop = {
  code: 2,
  stderr: "stanza: cannot read lp1.ts: too many levels of symbolic links\n",
  stdout: "",
};

function linkedParentTree(): string {
  const cwd = scratchGitRepository({
    files: { "a.ts": braced("root"), "real/a.ts": singleBlock },
  });

  mkdirSync(join(cwd, "real", "inner"));
  symlinkSync("real/inner", join(cwd, "link"));
  return cwd;
}

test("explain names a missing path with the supplied spelling", () => {
  const cwd = scratch("missing");
  expect(run({ cwd }, "explain", "nope.ts:3")).toEqual({
    code: 2,
    stderr: "stanza: no such file: nope.ts\n",
    stdout: "",
  });
});

test("explain names the invalid line using the supplied path", () => {
  const result = run("explain", "a.ts:0");
  expect(result.code).toBe(2);
  expect(result.stderr).toStartWith("stanza: a.ts:0: the line must be 1 or more\n");
});

test("explain picks the language from a symlink target, as --check does", () => {
  const dir = scratch("cli");
  writeFileSync(join(dir, "target.js"), "const x: number = 1;\n");
  symlinkSync("target.js", join(dir, "link.ts"));

  const result = run({ cwd: dir }, "explain", "link.ts:1");
  expect(result.code).toBe(2);
  expect(result.stderr).toStartWith("stanza: target.js:1 does not parse:");
  expect(run({ cwd: dir }, "--check", "link.ts").code).toBe(2);
});

test("explain refuses a path through a file with the line --check prints", () => {
  const cwd = scratchGitRepository({ files: { "dirty.ts": singleBlock } });
  const missing = { code: 2, stderr: "stanza: no such file: dirty.ts/x.ts\n", stdout: "" };
  expect(run({ cwd }, "--check", "dirty.ts/x.ts")).toEqual(missing);
  expect(run({ cwd }, "explain", "dirty.ts/x.ts:1")).toEqual(missing);
});

test("explain reads a symlink loop as unreadable, not missing", () => {
  const dir = scratch("cli");
  symlinkSync("lp2.ts", join(dir, "lp1.ts"));
  symlinkSync("lp1.ts", join(dir, "lp2.ts"));
  expect(run({ cwd: dir }, "explain", "lp1.ts:1")).toEqual(symlinkLoop);
});

test("explain skips a symlinked file when --check skips its target", () => {
  const cwd = scratchGitRepository({ files: { "api.gen.ts": singleBlock } });
  symlinkSync("api.gen.ts", join(cwd, "alias.ts"));

  const target = run({ cwd }, "explain", "api.gen.ts:1");
  expect(target.code).toBe(0);
  expect(target.stdout).toEndWith(
    "note: --fix and --check skip this file as generated or excluded\n",
  );

  expect(run({ cwd }, "explain", "alias.ts:1")).toEqual(target);

  writeFileSync(join(cwd, "script"), singleBlock);
  symlinkSync("script", join(cwd, "link.ts"));
  expect(run({ cwd }, "--check", "link.ts").stderr).toContain("its files are always skipped");

  const linked = run({ cwd }, "explain", "link.ts:1");
  expect(linked.code).toBe(0);
  expect(linked.stdout).toEndWith(
    "note: --fix and --check skip this file as generated or excluded\n",
  );
});

test("explain keeps a symlinked file when --check keeps its target", () => {
  const cwd = scratchGitRepository({
    files: { "a.ts": singleBlock, ".gitattributes": "alias.ts -stanza\n" },
  });

  symlinkSync("a.ts", join(cwd, "alias.ts"));
  expect(run({ cwd }, "--check", "alias.ts")).toEqual({
    code: 1,
    stdout: bracesAt("a.ts"),
    stderr: "1 finding in 1 file, fixable with --fix\n",
  });

  const explained = run({ cwd }, "explain", "alias.ts:1");
  expect(explained).toEqual(run({ cwd }, "explain", "a.ts:1"));
  expect(explained.code).toBe(0);
  expect(explained.stdout).not.toContain("skip this file");

  writeFileSync(join(cwd, ".gitattributes"), "a.ts -stanza\n");
  expect(run({ cwd }, "explain", "alias.ts:1").stdout).toEndWith(
    "note: --fix and --check skip this file, its stanza attribute opts it out\n",
  );
});

test("explain warns that git is missing as --check does", () => {
  const cwd = scratch("explain-no-git");
  writeFileSync(join(cwd, "a.ts"), singleBlock);

  const warning = "stanza: git not found on PATH, file selection fell back to the directory walk\n";
  const env = { PATH: scratch("empty-path") };
  expect(run({ cwd, env }, "--check", "a.ts").stderr).toStartWith(warning);

  const explained = run({ cwd, env }, "explain", "a.ts:1");
  expect(explained.code).toBe(0);
  expect(explained.stderr).toBe(warning);
});

test("explain steps up out of a symlinked folder as --check does", () => {
  const cwd = linkedParentTree();
  const real = run({ cwd }, "explain", "real/a.ts:1");
  expect(real.code).toBe(0);
  expect(run({ cwd }, "explain", "link/../a.ts:1")).toEqual(real);
});
